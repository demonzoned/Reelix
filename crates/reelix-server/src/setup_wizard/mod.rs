//! `reelix setup`: the install wizard in a terminal, for a server with no desktop.
//!
//! It asks the same questions as the browser wizard at `/setup`, in the same order, from
//! [`reelix_api_types::install`], and saves each answer through Reelix's own API on this server
//! (so a running Reelix is needed: `docker exec -it reelix /app/reelix setup`). From the server
//! itself no set-up code is needed.
//!
//! Two ways to draw it, one set of words: boxed (a cleared screen per step, a heading box, a
//! progress bar and a menu on the bottom line) and plain (`--plain` or `REELIX_PLAIN=1`: numbered
//! lines that scroll with nothing redrawn, for screen readers).

mod api;
pub mod feedback;
pub mod term;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// How long `reelix setup` waits for Reelix to answer before it stops.
const WAIT_FOR_REELIX: Duration = Duration::from_secs(120);

use reelix_api_types::content::{ContentKind, ContentRootFolderDto, CreateContentRootFolderRequest};
use reelix_api_types::install::{
    self, Answers, Context, FEEDBACK_KINDS, Help, PLACEHOLDER_NOTE, Question, Start, StepId,
};
use reelix_api_types::library::{CreateRootFolderRequest, RootFolderDto};
use reelix_api_types::migrate::{
    MigrateAction, MigrateConnection, MigratePlan, MigrateRunDto, MigrateRunRequest, MigrateState, SourceKind,
};
use reelix_api_types::providers::{
    MetadataSettingsDto, ProviderDto, ProviderInput, ProviderKind, ProviderSchemaDto, ProviderTestRequest,
    TestResultDto,
};
use reelix_api_types::requests::RequestSettingsDto;
use reelix_api_types::system::PendingRestoreDto;
use reelix_api_types::{LoginRequest, SetupRequest, SetupStatus, UserDto};
use reelix_core::provider::FieldKind;
use reelix_core::{MediaKind, Protocol};
use reelix_decision::{DelayProfile, QualityProfile};
use serde_json::{Map, Value};

use self::api::Api;
use self::feedback::Report;
use self::term::{Head, Key, Pick, Screen, Ui};
use crate::config::Config;

/// Options from the command line.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub plain: bool,
    /// Reelix's address; this server's own port when empty.
    pub url: Option<String>,
}

/// What the person typed at a question.
enum Input<T> {
    Got(T),
    Back,
    Stop,
}

/// How a step ended.
enum Flow {
    Next,
    Skip,
    Back,
    Stop,
}

/// Unwraps an answer, or ends the step with Back or Stop.
macro_rules! got {
    ($e:expr) => {
        match $e {
            Input::Got(v) => v,
            Input::Back => return Flow::Back,
            Input::Stop => return Flow::Stop,
        }
    };
}

struct Wizard {
    ui: Ui,
    api: Api,
    rt: tokio::runtime::Runtime,
    answers: Answers,
    ctx: Context,
    step: StepId,
    /// The step's paragraphs so far (boxed mode redraws them under each new question, wrapped to
    /// the column at that moment).
    body: Vec<String>,
    last_error: Option<String>,
    /// The set-up code and the passwords typed before, kept after Back without showing them.
    code: String,
    secrets: std::collections::HashMap<&'static str, String>,
    skipped: Vec<StepId>,
    /// Back stops here: the account (or the restore) changed something.
    floor: Option<StepId>,
    signed_in: bool,
    came_back: bool,
    version: String,
    feedback_dir: PathBuf,
    port: u16,
}

/// Whether `base` is plain http to a computer other than this one.
fn plain_http_elsewhere(base: &str) -> bool {
    let Some(rest) = base.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => host.rsplit_once(':').map_or(host, |(h, _)| h),
    };
    !(host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))
}

/// Runs the wizard; returns the process exit code.
pub fn run(config: &Config, opts: Options) -> ExitCode {
    let env_plain = std::env::var("REELIX_PLAIN").is_ok_and(|v| !v.is_empty() && v != "0");
    let ui = Ui::detect(opts.plain || env_plain);
    let base = opts.url.unwrap_or_else(|| {
        let addr = crate::healthcheck::target_addr(config.bind, config.port);
        format!("http://{addr}{}", config.url_base)
    });
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("reelix setup: could not start: {e}");
            return ExitCode::FAILURE;
        }
    };
    if plain_http_elsewhere(&base) {
        eprintln!(
            "Note: {base} is another computer over plain http, so the password, the set-up code and API keys \
             cross the network unencrypted. Use https, or run reelix setup on the server itself."
        );
    }
    let api = match Api::new(&base) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("reelix setup: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Straight after `docker compose up -d`, or a restart after a restore, Reelix takes a little
    // while to answer: wait for it, up to two minutes, instead of stopping at once.
    let began = Instant::now();
    let mut waited = false;
    let status = loop {
        match rt.block_on(api.get::<SetupStatus>("/setup")) {
            Ok(s) => {
                if waited && !ui.plain {
                    eprintln!();
                }
                break s;
            }
            Err(e) if began.elapsed() >= WAIT_FOR_REELIX => {
                if waited && !ui.plain {
                    eprintln!();
                }
                eprintln!(
                    "Reelix is not answering at {base} ({}). Start Reelix first, then run reelix setup again.",
                    e.message
                );
                return ExitCode::FAILURE;
            }
            Err(_) => {
                let secs = began.elapsed().as_secs();
                if ui.plain {
                    if !waited {
                        eprintln!(
                            "Waiting for Reelix to answer at {base}. This can take a minute after it starts."
                        );
                    }
                } else {
                    eprint!(
                        "\r\x1b[2KWaiting for Reelix to answer at {base}: {} min {:02} s",
                        secs / 60,
                        secs % 60
                    );
                }
                waited = true;
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    };
    let mut w = Wizard {
        ui,
        api,
        rt,
        answers: Answers::with_defaults(),
        ctx: Context {
            code_required: status.setup_token_required,
            signed_in: false,
        },
        step: StepId::Start,
        body: Vec::new(),
        last_error: None,
        code: String::new(),
        secrets: Default::default(),
        skipped: Vec::new(),
        floor: None,
        signed_in: false,
        came_back: false,
        version: status.version,
        feedback_dir: config.data_dir.join("feedback"),
        port: config.port,
    };
    if !status.needs_setup && !w.sign_in() {
        w.ui.release();
        return ExitCode::FAILURE;
    }
    w.run_steps()
}

impl Wizard {
    fn shown(&self) -> Vec<StepId> {
        install::steps_shown(&self.answers, self.ctx)
    }

    fn run_steps(&mut self) -> ExitCode {
        let mut id = StepId::Start;
        loop {
            self.step = id;
            let flow = self.run_step(id);
            let shown = self.shown();
            let pos = shown.iter().position(|s| *s == id).unwrap_or(0);
            match flow {
                Flow::Next | Flow::Skip => {
                    if matches!(flow, Flow::Skip) {
                        self.skipped.push(id);
                    } else {
                        self.skipped.retain(|s| *s != id);
                    }
                    self.came_back = false;
                    match shown.get(pos + 1) {
                        Some(n) => id = *n,
                        None => {
                            self.ui.release();
                            return ExitCode::SUCCESS;
                        }
                    }
                }
                Flow::Back => {
                    let floor = self.floor.and_then(|f| shown.iter().position(|s| *s == f));
                    if pos > 0 && floor.is_none_or(|f| pos > f + 1) {
                        id = shown[pos - 1];
                        self.came_back = true;
                    } else {
                        self.last_error = Some(
                            "This step cannot go back: the step before it is saved. Change it later in Settings."
                                .into(),
                        );
                    }
                }
                Flow::Stop => {
                    self.ui.release();
                    self.ui.refresh();
                    let words = "Stopped. Nothing else was changed. To carry on later, run reelix setup again \
                                 (with Docker: docker exec -it reelix /app/reelix setup).";
                    for l in term::wrap(words, self.ui.width()) {
                        println!("{}{l}", " ".repeat(self.ui.left()));
                    }
                    return ExitCode::SUCCESS;
                }
            }
        }
    }

    fn head(&self) -> Head {
        let shown = self.shown();
        let n = shown.iter().position(|s| *s == self.step).unwrap_or(0) + 1;
        Head {
            title: install::step(self.step).title.to_owned(),
            n,
            total: shown.len(),
        }
    }

    /// Starts a step: a new screen (boxed) or a heading line and the keys (plain).
    fn begin(&mut self) {
        self.ui.refresh();
        self.body.clear();
        let s = install::step(self.step);
        if self.ui.plain {
            println!();
            for l in self.ui.heading(&self.head()) {
                self.print_plain(&l);
            }
            self.print_plain("Type b to go back, ? for help, f to send feedback or q to stop.");
        }
        self.say(&self.ui.bold(s.question));
        if self.came_back {
            self.say("You came back to this step.");
        }
        self.say(s.lead);
        if let Some(e) = self.last_error.take() {
            let line = self.ui.danger(&e);
            self.say(&line);
        }
    }

    /// Adds a paragraph to the step (and prints it in plain mode). It is kept whole, and wrapped
    /// to the column each time it is drawn.
    fn say(&mut self, text: &str) {
        if self.ui.plain {
            self.print_plain(text);
        }
        self.body.push(String::new());
        self.body.push(text.to_owned());
    }

    /// Prints text in plain mode, wrapped between words at the window's width.
    fn print_plain(&self, text: &str) {
        for l in term::wrap(&term::strip_ansi(text), self.ui.width()) {
            println!("{l}");
        }
    }

    /// The step as it is now, with `question` and `prompt` last.
    fn screen(&self, question: &[String], prompt: &str, enter: &str) -> Screen {
        let mut body = self.body.clone();
        body.push(String::new());
        body.extend(question.iter().cloned());
        Screen {
            head: self.head(),
            body,
            pick: None,
            prompt: prompt.to_owned(),
            cut: false,
            keys: term::STEP_KEYS.to_owned(),
            enter: enter.to_owned(),
        }
    }

    /// Shows `question` (lines) and reads an answer; handles ?, f, b and q itself.
    fn ask(&mut self, question: &[String], prompt: &str, enter: &str, secret: bool) -> Input<String> {
        let mut printed = false;
        loop {
            if self.ui.plain {
                if !printed {
                    for l in question {
                        self.print_plain(l);
                    }
                    printed = true;
                }
                print!("{prompt}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            } else {
                self.ui.refresh();
                self.ui.show(self.screen(question, prompt, enter));
            }
            let line = if secret {
                term::read_secret()
            } else {
                term::read_line()
            };
            let Some(line) = line else {
                return Input::Stop;
            };
            match line.trim() {
                "b" | "B" => return Input::Back,
                "q" | "Q" => return Input::Stop,
                "?" => self.show_help(question),
                "f" | "F" => self.send_feedback(None),
                _ => return Input::Got(line),
            }
        }
    }

    /// One of a question's answers, or several with commas.
    fn choose(&mut self, q: &Question) -> Input<String> {
        // Only the answers on offer here, numbered in order.
        let choices: Vec<&install::Choice> = q
            .choices
            .iter()
            .filter(|c| install::offered(q, c.value, self.ctx))
            .collect();
        let current = self.answers.get(q.key).to_owned();
        let mut default: Vec<usize> = choices
            .iter()
            .enumerate()
            .filter(|(_, c)| current.split(',').any(|v| v == c.value))
            .map(|(i, _)| i + 1)
            .collect();
        if default.is_empty() {
            default.push(1);
        }
        let default_text = default
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut lines = vec![self.ui.bold(q.legend)];
        for (i, c) in choices.iter().enumerate() {
            let mark = if c.recommended { "●" } else { " " };
            let rec = if c.recommended { " (recommended)" } else { "" };
            lines.push(format!(" {:>2}. {mark} {}{rec}", i + 1, c.label));
            lines.push(self.ui.dim(&format!("       {}", c.hint)));
        }
        let prompt = if q.multi {
            format!("› Your choices, as numbers with commas [{default_text}]: ")
        } else {
            format!("› Your choice [{default_text}]: ")
        };
        let enter = if self.came_back { "keep" } else { "recommended" };
        // Several answers: a list with marks in a real terminal with the boxed look; typed
        // numbers in plain mode, or when the list does not fit the window.
        let mut ticked = None;
        if q.multi && self.ui.keys_work() {
            self.ui.refresh();
            let pick = Pick {
                items: choices
                    .iter()
                    .map(|c| {
                        let rec = if c.recommended { " (recommended)" } else { "" };
                        (format!("{}{rec}", c.label), c.hint.to_owned())
                    })
                    .collect(),
                ticked: (1..=choices.len()).map(|n| default.contains(&n)).collect(),
                at: 0,
                note: term::PICK_NOTE.to_owned(),
            };
            let room = self
                .ui
                .rows
                .saturating_sub(self.ui.heading(&self.head()).len() + 3);
            if self.ui.pick_lines(&pick).len() + 2 <= room {
                ticked = self.pick_many(q, pick, enter);
            }
        }
        loop {
            let picked = match ticked.take() {
                Some(Input::Got(p)) => Some(p),
                Some(Input::Back) => return Input::Back,
                Some(Input::Stop) => return Input::Stop,
                None => {
                    let line = match self.ask(&lines, &prompt, enter, false) {
                        Input::Got(l) => l,
                        Input::Back => return Input::Back,
                        Input::Stop => return Input::Stop,
                    };
                    if line.trim().is_empty() {
                        Some(default.clone())
                    } else {
                        parse_numbers(&line, choices.len())
                    }
                }
            };
            match picked {
                Some(p) if !p.is_empty() && (q.multi || p.len() == 1) => {
                    let values: Vec<&str> = p.iter().map(|n| choices[n - 1].value).collect();
                    let labels: Vec<&str> = p.iter().map(|n| choices[n - 1].label).collect();
                    self.answers.set(q.key, values.join(","));
                    self.say(&format!("{}: {}", q.legend, labels.join(", ")));
                    return Input::Got(values.join(","));
                }
                _ => {
                    let msg = if q.multi {
                        format!(
                            "Type numbers from 1 to {}, with commas, such as 1,2.",
                            choices.len()
                        )
                    } else {
                        format!("Type one number from 1 to {}.", choices.len())
                    };
                    lines.push(self.ui.danger(&msg));
                    if self.ui.plain {
                        self.print_plain(&msg);
                    }
                }
            }
        }
    }

    /// Several answers in a list with marks: Up and Down move, Space ticks or unticks, Enter
    /// carries on, and a typed number ticks that line. ? opens help, f feedback, b goes back and
    /// q (or Ctrl+C) stops. The list is drawn in place, so it never repeats down the window.
    /// The numbers ticked, from 1, or `None` when keys cannot be read one at a time here.
    fn pick_many(&mut self, q: &Question, mut pick: Pick, enter: &str) -> Option<Input<Vec<usize>>> {
        let n = pick.items.len();
        let legend = vec![self.ui.bold(q.legend)];
        loop {
            let mut screen = self.screen(&legend, "", enter);
            screen.pick = Some(pick.clone());
            self.ui.show(screen);
            // The terminal goes back to how it was when `keys` is dropped: on every return, and
            // before help or feedback is drawn.
            let keys = term::Keys::start().ok()?;
            let help = 'keys: loop {
                for key in keys.read() {
                    pick.note = term::PICK_NOTE.to_owned();
                    match key {
                        Key::Up => pick.at = (pick.at + n - 1) % n,
                        Key::Down => pick.at = (pick.at + 1) % n,
                        Key::Space => pick.ticked[pick.at] ^= true,
                        Key::Char(c @ '1'..='9') => {
                            let i = c as usize - '1' as usize;
                            if i < n {
                                pick.at = i;
                                pick.ticked[i] ^= true;
                            }
                        }
                        Key::Enter => {
                            let picked: Vec<usize> = (1..=n).filter(|i| pick.ticked[i - 1]).collect();
                            if !picked.is_empty() {
                                return Some(Input::Got(picked));
                            }
                            pick.note = "Tick at least one with Space, then press Enter.".to_owned();
                        }
                        Key::Char('b' | 'B') => return Some(Input::Back),
                        Key::Char('q' | 'Q') | Key::Stop => return Some(Input::Stop),
                        Key::Char('?') => break 'keys true,
                        Key::Char('f' | 'F') => break 'keys false,
                        _ => {}
                    }
                }
                self.ui.update_pick(pick.clone());
            };
            drop(keys);
            if help {
                self.show_help(&legend);
            } else {
                self.send_feedback(None);
            }
        }
    }

    /// A free answer. `secret` hides it as it is typed; Enter keeps the one typed before.
    fn text(
        &mut self,
        label: &str,
        hint: &str,
        default: &str,
        secret: Option<&'static str>,
    ) -> Input<String> {
        let mut lines = vec![self.ui.bold(label)];
        if !hint.is_empty() {
            lines.push(self.ui.dim(hint));
        }
        let kept = secret.and_then(|k| self.secrets.get(k)).cloned();
        let prompt = match (&kept, secret) {
            (Some(_), _) => {
                "› Type it again, or press Enter to keep the one you typed (nothing shows as you type): "
                    .to_owned()
            }
            (None, Some(_)) => "› (nothing shows as you type): ".to_owned(),
            (None, None) if default.is_empty() => "› ".to_owned(),
            (None, None) => format!("› [{default}]: "),
        };
        let enter = if kept.is_some() { "keep" } else { "next" };
        let line = match self.ask(&lines, &prompt, enter, secret.is_some()) {
            Input::Got(l) => l,
            other => return other,
        };
        let value = match (line.trim().is_empty(), kept) {
            (true, Some(k)) => k,
            (true, None) => default.to_owned(),
            (false, _) => line.trim().to_owned(),
        };
        if let Some(k) = secret {
            self.secrets.insert(k, value.clone());
        }
        Input::Got(value)
    }

    fn show_help(&mut self, question: &[String]) {
        self.ui.refresh();
        let s = install::step(self.step);
        let mut text: Vec<String> = Vec::new();
        // Every step says how to choose plain mode, unless its own help already does.
        let plain_said = s.help.iter().any(|h| h.term.contains("screen reader"));
        let extra = (!plain_said).then_some(&PLAIN_HELP);
        for Help { term, text: paras } in s.help.iter().chain(extra) {
            text.push(self.ui.bold(term));
            text.extend(paras.iter().map(|p| (*p).to_owned()));
            text.push(String::new());
        }
        text.push("The Reelix help centre is not online yet, so there is no link here.".into());
        if self.ui.plain {
            println!();
            for l in &text {
                for w in term::wrap(&term::strip_ansi(l), self.ui.width()) {
                    println!("{w}");
                }
            }
            println!();
            return;
        }
        let title = format!("Help: {}", s.question);
        let mut behind = self.ui.heading(&self.head());
        behind.extend(self.body.iter().cloned());
        behind.extend(question.iter().cloned());
        // A page at a time when the help is longer than the window.
        let room = self.ui.rows.saturating_sub(6).max(4);
        let wrapped: Vec<String> = text
            .iter()
            .flat_map(|l| term::wrap(l, self.ui.overlay_width() - 4))
            .collect();
        let pages: Vec<&[String]> = wrapped.chunks(room).collect();
        for (i, page) in pages.iter().enumerate() {
            let last = i + 1 == pages.len();
            let menu = self
                .ui
                .menu_line("", if last { "Close help" } else { "more below" });
            self.ui.overlay(&behind, &title, page, &menu);
            if term::read_line().is_none() {
                break;
            }
        }
    }

    /// The terminal version of Snaglist: kind, one or two questions, the whole report, then how to
    /// send it.
    fn send_feedback(&mut self, error: Option<String>) {
        let s = install::step(self.step);
        let error = error.or_else(|| self.last_error.clone());
        let mut lines = vec![self.ui.bold("What do you want to tell us?")];
        for (i, k) in FEEDBACK_KINDS.iter().enumerate() {
            lines.push(format!(" {:>2}. {}", i + 1, k.label));
        }
        let default = if error.is_some() { 1 } else { 2 };
        let kind = loop {
            let reply = self.feedback_prompt(&lines, &format!("› Your choice [{default}]: "));
            let Some(reply) = reply else { return };
            let n = if reply.trim().is_empty() {
                Some(vec![default])
            } else {
                parse_numbers(&reply, FEEDBACK_KINDS.len())
            };
            if let Some([n]) = n.as_deref() {
                break &FEEDBACK_KINDS[*n - 1];
            }
        };
        let (ask1, ask2) = match kind.value {
            "bug" => ("What happened?", Some("What did you expect?")),
            "feature" => (
                "What would you like to be able to do?",
                Some("What would it help you with?"),
            ),
            "idea" => ("What could be better?", Some("Your suggestion")),
            "other" => ("Your message", None),
            _ => ("What is wrong?", None),
        };
        let note = "Do not type passwords, keys or the set-up code here.";
        let Some(words) = self.feedback_prompt(&[self.ui.bold(ask1), self.ui.dim(note)], "› ") else {
            return;
        };
        let expected = match ask2 {
            Some(q) => match self.feedback_prompt(
                &[self.ui.bold(q), self.ui.dim("Press Enter to leave it out.")],
                "› ",
            ) {
                Some(e) => e,
                None => return,
            },
            None => String::new(),
        };
        let report = Report {
            kind: kind.value,
            kind_label: kind.label,
            step: s,
            words,
            expected,
            error,
            version: self.version.clone(),
        };
        // The whole report, before anything leaves this computer.
        let mut review: Vec<String> = vec![self.ui.bold("This is the whole report. Nothing has been sent.")];
        review.extend(report.markdown().lines().map(str::to_owned));
        review.push(String::new());
        review.push(self.ui.bold("How do you want to send it?"));
        let ways = if self.ui.plain {
            vec![
                " 1. ● Show the link (recommended)",
                " 2.   Save to a file",
                " 3.   Cancel",
            ]
        } else {
            vec![
                " 1. ● Show as a QR code (recommended)",
                " 2.   Show the link",
                " 3.   Save to a file",
                " 4.   Cancel",
            ]
        };
        review.extend(ways.iter().map(|w| (*w).to_owned()));
        let Some(choice) = self.feedback_prompt(&review, "› Your choice [1]: ") else {
            return;
        };
        let mut n: usize = choice.trim().parse().unwrap_or(1);
        if self.ui.plain {
            n += 1; // No QR code in plain mode: a screen reader cannot read it.
        }
        let saved = report.save(&self.feedback_dir);
        let saved_line = match &saved {
            Ok(p) => format!("The whole report is saved on the server: {}", p.display()),
            Err(e) => format!("The report could not be saved on the server: {e}"),
        };
        match n {
            1 => self.show_qr(&report, saved.as_ref().ok().map(|p| p.as_path()), &saved_line),
            2 => {
                let url = report.link(saved.as_ref().ok().map(|p| p.as_path()));
                let text = vec![
                    self.ui
                        .bold("Open this link on any device to post the report on GitHub:"),
                    url,
                    String::new(),
                    saved_line,
                ];
                let _ = self.feedback_prompt(&text, "› Press Enter when you are done: ");
            }
            3 => {
                let _ = self.feedback_prompt(&[saved_line], "› Press Enter to go back to the step: ");
            }
            _ => {
                if let Ok(p) = saved {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
    }

    /// Draws the QR code on a view of its own, sized to the window.
    fn show_qr(&mut self, report: &Report, saved: Option<&std::path::Path>, saved_line: &str) {
        self.ui.refresh();
        self.ui.quiet();
        let fit = feedback::largest_version(self.ui.cols, self.ui.rows)
            .and_then(|v| report.short_link(feedback::capacity(v), saved).map(|l| (v, l)));
        let Some((version, (url, cut))) = fit else {
            let _ = self.feedback_prompt(
                &[
                    self.ui.danger("Make the window larger, or use Show the link."),
                    saved_line.to_owned(),
                ],
                "› Press Enter to go back: ",
            );
            return;
        };
        let Some(code) = feedback::qr(&url, version) else {
            return;
        };
        loop {
            let mut out = String::from("\x1b[2J\x1b[H");
            out.push_str(
                &self
                    .ui
                    .bold("Scan it with your phone's camera to open the report there"),
            );
            out.push_str("\r\n");
            for l in feedback::draw(&code) {
                out.push_str(&l);
                out.push_str("\r\n");
            }
            let note = if cut {
                "Your words were cut to fit the code. The whole report is saved on the server."
            } else {
                "The whole report is saved on the server too."
            };
            out.push_str(&term::wrap(note, self.ui.cols).join("\r\n"));
            out.push_str(&format!(
                "\x1b[{};1H{}",
                self.ui.rows,
                self.ui.menu_line("l Show the link", "Done")
            ));
            print!("{out}");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            match term::read_line() {
                Some(l) if l.trim().eq_ignore_ascii_case("l") => {
                    let _ = self.feedback_prompt(
                        &[
                            self.ui.bold("The link in the code:"),
                            url.clone(),
                            String::new(),
                            saved_line.to_owned(),
                        ],
                        "› Press Enter to see the code again: ",
                    );
                }
                _ => return,
            }
        }
    }

    /// A question inside the feedback flow: an overlay (boxed) or lines (plain). `None` on q.
    fn feedback_prompt(&mut self, lines: &[String], prompt: &str) -> Option<String> {
        if self.ui.plain {
            for l in lines {
                self.print_plain(l);
            }
            print!("{prompt}");
        } else {
            self.ui.refresh();
            let mut behind = self.ui.heading(&self.head());
            behind.extend(self.body.iter().cloned());
            self.ui.overlay(
                &behind,
                "Send feedback",
                lines,
                &self.ui.menu_line("q Cancel", "next"),
            );
            let row = self.ui.rows.saturating_sub(1);
            print!("\x1b[{row};1H\x1b[2K\x1b[{row};{}H{prompt}", self.ui.left() + 1);
        }
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let line = term::read_line()?;
        (!line.trim().eq_ignore_ascii_case("q")).then_some(line)
    }

    /// Calls the API; on failure, keeps the error for the next screen and for feedback.
    fn call<T>(&mut self, res: Result<T, api::ApiError>) -> Option<T> {
        match res {
            Ok(v) => Some(v),
            Err(e) => {
                let msg = format!("Reelix could not save this: {}", e.message);
                let line = self.ui.danger(&msg);
                self.say(&line);
                self.say("Type f to report this problem, with the error already in the report.");
                self.last_error = Some(msg);
                None
            }
        }
    }

    /// Asks the step's own questions, in order.
    fn questions(&mut self) -> Option<Flow> {
        let s = install::step(self.step);
        for q in s.questions {
            if !q.asked(&self.answers) {
                continue;
            }
            match self.choose(q) {
                Input::Got(_) => {}
                Input::Back => return Some(Flow::Back),
                Input::Stop => return Some(Flow::Stop),
            }
        }
        None
    }

    fn run_step(&mut self, id: StepId) -> Flow {
        self.begin();
        match id {
            StepId::Start => {
                if let Some(f) = self.questions() {
                    return f;
                }
                if self.signed_in && self.answers.start() == Start::Restore {
                    self.last_error = Some(
                        "Reelix is already set up, so a backup is restored in System, then Backup.".into(),
                    );
                    return self.run_step(id);
                }
                Flow::Next
            }
            StepId::Code => {
                let code = got!(self.text(
                    "Set-up code",
                    "3 groups of letters and numbers, from the installer's output or docker logs reelix.",
                    "",
                    Some("code"),
                ));
                self.code = code;
                Flow::Next
            }
            StepId::Account => self.account(),
            StepId::Restore => self.restore(),
            StepId::Follow => {
                if let Some(f) = self.questions() {
                    return f;
                }
                if self.answers.get("follow").is_empty() {
                    self.last_error = Some("Choose at least one.".into());
                    return self.run_step(id);
                }
                Flow::Next
            }
            StepId::Migrate => self.migrate(),
            StepId::Tmdb => self.tmdb(),
            StepId::Folders => self.folders(),
            StepId::Torrent | StepId::Usenet => self.download_app(id),
            StepId::Sites => self.sites(),
            StepId::MediaServers => self.media_servers(),
            StepId::DownloadOptions | StepId::Quality | StepId::Monitor | StepId::Requests => {
                if let Some(f) = self.questions() {
                    return f;
                }
                if self.save_answers(id) {
                    Flow::Next
                } else {
                    self.run_step(id)
                }
            }
            StepId::Finish => {
                self.finish();
                Flow::Next
            }
        }
    }

    fn sign_in(&mut self) -> bool {
        self.ui.refresh();
        println!("Reelix is already set up. Sign in with an admin account to change the set-up.");
        for _ in 0..3 {
            print!("Username: ");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            let Some(username) = term::read_line() else {
                return false;
            };
            print!("Password (nothing shows as you type): ");
            let Some(password) = term::read_secret() else {
                return false;
            };
            let req = LoginRequest {
                username: username.trim().to_owned(),
                password,
                remember_me: false,
            };
            match self.rt.block_on(self.api.post::<_, UserDto>("/auth/login", &req)) {
                Ok(_) => {
                    self.signed_in = true;
                    self.ctx.signed_in = true;
                    return true;
                }
                Err(e) => println!("{}", e.message),
            }
        }
        false
    }

    fn account(&mut self) -> Flow {
        let before = self.secrets.get("username").cloned().unwrap_or_default();
        let username = got!(self.text(
            "Username",
            "Letters, numbers, dots, dashes and underscores.",
            &before,
            None,
        ));
        self.secrets.insert("username", username.clone());
        let password = got!(self.text(
            "Password",
            "At least 8 characters. A short sentence works well.",
            "",
            Some("password"),
        ));
        let confirm = got!(self.text("Enter the password again", "", "", Some("confirm")));
        if password.chars().count() < 8 {
            self.last_error = Some("The password needs at least 8 characters.".into());
            self.secrets.remove("password");
            self.secrets.remove("confirm");
            return self.run_step(StepId::Account);
        }
        if password != confirm {
            self.last_error = Some("The passwords are different. Enter it again.".into());
            self.secrets.remove("confirm");
            return self.run_step(StepId::Account);
        }
        let req = SetupRequest {
            username,
            password,
            email: None,
            setup_token: Some(self.code.trim().to_owned()).filter(|c| !c.is_empty()),
        };
        match self.rt.block_on(self.api.post::<_, UserDto>("/setup", &req)) {
            Ok(user) => {
                self.secrets.remove("password");
                self.secrets.remove("confirm");
                self.floor = Some(StepId::Account);
                self.say(&format!(
                    "Admin account {} created. You are signed in.",
                    user.username
                ));
                Flow::Next
            }
            Err(e) => {
                self.last_error = Some(match e.status {
                    403 => "The set-up code is not right. Copy it again from the installer's output.".into(),
                    _ => format!("The account could not be made: {}", e.message),
                });
                self.run_step(StepId::Account)
            }
        }
    }

    fn restore(&mut self) -> Flow {
        let path = got!(self.text(
            "Backup file",
            "The full path on this server, such as /data/backups/reelix-backup-2026-11-01.zip.",
            "",
            None,
        ));
        // The file is sent as it is read: a backup can be up to 2 GiB, too much to hold in memory.
        let opened = self.rt.block_on(async {
            let file = tokio::fs::File::open(path.trim()).await?;
            let len = file.metadata().await?.len();
            Ok::<_, std::io::Error>((file, len))
        });
        let (file, len) = match opened {
            Ok(v) => v,
            Err(e) => {
                self.last_error = Some(format!("Reelix cannot read that file: {e}"));
                return self.run_step(StepId::Restore);
            }
        };
        if len > MAX_BACKUP_BYTES {
            self.last_error = Some("That file is larger than 2 GiB, the most Reelix restores.".into());
            return self.run_step(StepId::Restore);
        }
        let code = self.code.trim().to_owned();
        let res =
            self.rt.block_on(
                self.api
                    .post_file::<PendingRestoreDto>("/setup/restore", file, len, &code),
            );
        match res {
            Ok(_) => {
                self.floor = Some(StepId::Restore);
                Flow::Next
            }
            Err(e) => {
                self.last_error = Some(format!("The backup could not be restored: {}", e.message));
                self.run_step(StepId::Restore)
            }
        }
    }

    fn migrate(&mut self) -> Flow {
        loop {
            let mut lines = vec![self.ui.bold("Which app do you want to bring over?")];
            for (i, s) in SourceKind::ALL.iter().enumerate() {
                lines.push(format!(" {:>2}. {}", i + 1, s.label()));
            }
            let done_n = SourceKind::ALL.len() + 1;
            lines.push(format!(
                " {done_n:>2}. ● That is all (recommended once each app is done)"
            ));
            let reply = got!(self.ask(&lines, &format!("› Your choice [{done_n}]: "), "next step", false));
            let n = if reply.trim().is_empty() {
                done_n
            } else {
                match parse_numbers(&reply, done_n).as_deref() {
                    Some([n]) => *n,
                    _ => continue,
                }
            };
            if n == done_n {
                return Flow::Next;
            }
            let source = SourceKind::ALL[n - 1];
            let url = got!(self.text(
                &format!("{}'s address", source.label()),
                "Such as http://192.168.1.10:7878, with any URL base.",
                "",
                None,
            ));
            // Each app has its own key: "press Enter to keep it" must never offer one app's key to
            // another. It is kept only while this app is being brought over.
            let key_slot = arr_key_slot(source);
            let api_key = got!(self.text(
                &format!("{}'s API key", source.label()),
                "In the app: Settings, then General, then Security.",
                "",
                Some(key_slot),
            ));
            let connection = MigrateConnection {
                source,
                url,
                api_key,
                path_rewrites: Vec::new(),
                secrets: Default::default(),
            };
            let Some(plan) = self.call(
                self.rt.block_on(
                    self.api
                        .post::<_, MigratePlan>("/system/migrate/preview", &connection),
                ),
            ) else {
                continue;
            };
            let count = |a: MigrateAction| plan.items.iter().filter(|i| i.action == a).count();
            self.say(&format!(
                "{} {}: {} to add, {} already in Reelix, {} left out.",
                source.label(),
                term::clean(&plan.version),
                count(MigrateAction::Create),
                count(MigrateAction::Existing),
                count(MigrateAction::Skip)
            ));
            for note in &plan.notes {
                self.say(&format!("  {}", term::clean(note)));
            }
            let lines = vec![
                self.ui.bold("Bring it over?"),
                "  1. ● Bring it over (recommended)".to_owned(),
                "  2.   Not this one".to_owned(),
            ];
            let reply = got!(self.ask(&lines, "› Your choice [1]: ", "recommended", false));
            if reply.trim() == "2" {
                self.secrets.remove(key_slot);
                continue;
            }
            let req = MigrateRunRequest {
                connection,
                categories: Vec::new(),
                search_missing: false,
            };
            // One progress line, drawn in place. When the run does not finish, the line stays,
            // marked, with the problem and the choices under it; trying again draws the step
            // afresh with one new line, not a second one under the first.
            let kept = self.body.len();
            loop {
                self.body.truncate(kept);
                if self
                    .call(
                        self.rt
                            .block_on(self.api.post::<_, MigrateRunDto>("/system/migrate/run", &req)),
                    )
                    .is_none()
                {
                    break;
                }
                match self.follow_run(source.label()) {
                    Ok(run) => {
                        let made = run
                            .items
                            .iter()
                            .filter(|i| i.action == MigrateAction::Create)
                            .count();
                        let failed = run
                            .items
                            .iter()
                            .filter(|i| i.action == MigrateAction::Failed)
                            .count();
                        self.last_error = None;
                        self.say(&format!(
                            "✔ {} is brought over: {made} added, {failed} could not be added.",
                            source.label()
                        ));
                        break;
                    }
                    Err(e) => {
                        let mark = self
                            .ui
                            .danger(&format!("✘ Bringing over {} did not finish", source.label()));
                        self.say(&mark);
                        self.say(&e);
                        self.last_error = Some(e);
                        let lines = vec![
                            self.ui.bold("What now?"),
                            "  1. ● Try again (recommended)".to_owned(),
                            "  2.   Choose another app, or carry on".to_owned(),
                        ];
                        let reply = got!(self.ask(&lines, "› Your choice [1]: ", "recommended", false));
                        if reply.trim() == "2" {
                            break;
                        }
                    }
                }
            }
            self.secrets.remove(key_slot);
        }
    }

    /// Follows a run until it ends, on one line that turns while it waits: what is happening and
    /// how far it is, cut to the column. Plain mode prints the count when it changes instead.
    fn follow_run(&mut self, label: &str) -> Result<MigrateRunDto, String> {
        // Keys pressed while it waits are not shown (an Enter would push the line down).
        let _quiet = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            term::Quiet::start().ok()
        } else {
            None
        };
        let began = Instant::now();
        let mut turn = 0;
        let mut count = String::new();
        let mut printed = String::new();
        let mut drawn = false;
        loop {
            let words = format!("Bringing over {label}{count}. Please wait, this can take some time.");
            if self.ui.plain {
                if words != printed {
                    self.print_plain(&words);
                    printed = words;
                }
                std::thread::sleep(Duration::from_secs(1));
            } else {
                // A quarter turn at a time, asking Reelix how far it is once a second.
                for _ in 0..4 {
                    self.ui.refresh();
                    let line = self.job_line(&words, began, turn);
                    if drawn {
                        self.ui.update_prompt(line);
                    } else {
                        let mut screen = self.screen(&[], &line, "");
                        screen.cut = true;
                        screen.keys = String::new();
                        self.ui.show(screen);
                        drawn = true;
                    }
                    turn += 1;
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
            let run = self
                .rt
                .block_on(self.api.get::<Option<MigrateRunDto>>("/system/migrate"));
            match run {
                Err(e) => return Err(format!("Reelix did not answer: {}", e.message)),
                Ok(None) => return Err("Reelix has no record of the run.".to_owned()),
                Ok(Some(run)) if run.state == MigrateState::Running => {
                    count = format!(", {} of {} done", run.done, run.total);
                }
                Ok(Some(run)) if run.state == MigrateState::Failed || run.error.is_some() => {
                    return Err(run
                        .error
                        .as_deref()
                        .map(term::clean)
                        .unwrap_or_else(|| "Reelix stopped before the end.".to_owned()));
                }
                Ok(Some(run)) => return Ok(run),
            }
        }
    }

    /// A job's progress line: a turning mark, the words, and the time taken. The words are cut so
    /// the time stays in view and the line fits the column.
    fn job_line(&self, words: &str, began: Instant, turn: usize) -> String {
        let secs = began.elapsed().as_secs();
        let timer = format!("{} min {:02} s", secs / 60, secs % 60);
        let room = self.ui.width().saturating_sub(timer.len() + 5).max(10);
        let mark = ["◐", "◓", "◑", "◒"][turn % 4];
        format!(
            "  {} {} {}",
            self.ui.accent(mark),
            term::clip(words, room),
            self.ui.dim(&timer)
        )
    }

    fn tmdb(&mut self) -> Flow {
        self.say("1. Create a free account at https://www.themoviedb.org/signup");
        self.say("2. Go to Settings, then API, and request a key for personal use.");
        self.say("3. Copy the API Read Access Token, the longer of the two.");
        let token = got!(self.text(
            "Read access token",
            "Reelix keeps it on this server. Press Enter with nothing typed to skip this step.",
            "",
            Some("tmdb"),
        ));
        if token.is_empty() {
            return Flow::Skip;
        }
        let test = MetadataSettingsDto {
            tmdb_read_token: Some(token.clone()),
            ..MetadataSettingsDto::default()
        };
        match self.call(
            self.rt.block_on(
                self.api
                    .post::<_, TestResultDto>("/settings/metadata/test", &test),
            ),
        ) {
            Some(r) if r.ok => {}
            Some(_) => {
                self.last_error = Some("TMDB did not accept the token. Copy it again.".into());
                self.secrets.remove("tmdb");
                return self.run_step(StepId::Tmdb);
            }
            None => return self.run_step(StepId::Tmdb),
        }
        let api = &self.api;
        let saved = self.rt.block_on(async {
            let mut m = api.get::<MetadataSettingsDto>("/settings/metadata").await?;
            m.tmdb_read_token = Some(token);
            api.put::<_, MetadataSettingsDto>("/settings/metadata", &m).await
        });
        self.secrets.remove("tmdb");
        match saved {
            Ok(_) => Flow::Next,
            Err(e) => {
                self.last_error = Some(format!("Reelix could not save this: {}", e.message));
                self.run_step(StepId::Tmdb)
            }
        }
    }

    /// Library folders: one for each kind followed, from the list the browser wizard offers too
    /// ([`install::library_folders`]), each suggested inside `/data`.
    fn folders(&mut self) -> Flow {
        let api = &self.api;
        let existing = self.rt.block_on(async {
            let (media, books, music) = futures::join!(
                api.get::<Vec<RootFolderDto>>("/rootfolders"),
                api.get::<Vec<ContentRootFolderDto>>("/bookrootfolders"),
                api.get::<Vec<ContentRootFolderDto>>("/musicrootfolders"),
            );
            let mut have: Vec<(String, String)> = Vec::new();
            for f in media.unwrap_or_default() {
                let key = if f.media_kind == MediaKind::Movie {
                    "movie"
                } else {
                    "series"
                };
                have.push((key.to_owned(), f.path));
            }
            for f in books
                .unwrap_or_default()
                .into_iter()
                .chain(music.unwrap_or_default())
            {
                have.push((content_key(f.kind).to_owned(), f.path));
            }
            have
        });
        for (_, path) in &existing {
            self.say(&format!("Already added: {path}"));
        }
        let wanted = install::library_folders(&self.answers);
        if wanted.is_empty() {
            return Flow::Next;
        }
        for folder in wanted {
            if existing.iter().any(|(key, _)| key == folder.key) {
                continue;
            }
            let path = got!(self.text(
                &format!("Folder for {}", folder.label.to_lowercase()),
                "As Reelix sees it, inside /data. Type s to skip this one.",
                folder.path,
                None,
            ));
            if path.eq_ignore_ascii_case("s") {
                continue;
            }
            let api = &self.api;
            let added = self.rt.block_on(async {
                match folder.key {
                    "movie" | "series" => {
                        let req = CreateRootFolderRequest {
                            path,
                            media_kind: if folder.key == "movie" {
                                MediaKind::Movie
                            } else {
                                MediaKind::Series
                            },
                        };
                        api.post::<_, RootFolderDto>("/rootfolders", &req)
                            .await
                            .map(|_| ())
                    }
                    key => {
                        let (kind, endpoint) = match key {
                            "ebook" => (ContentKind::Ebook, "/bookrootfolders"),
                            "audiobook" => (ContentKind::Audiobook, "/bookrootfolders"),
                            _ => (ContentKind::Music, "/musicrootfolders"),
                        };
                        let req = CreateContentRootFolderRequest { path, kind };
                        api.post::<_, ContentRootFolderDto>(endpoint, &req)
                            .await
                            .map(|_| ())
                    }
                }
            });
            if self.call(added).is_none() {
                return self.run_step(StepId::Folders);
            }
            self.say(&format!("{} folder added.", folder.label));
        }
        Flow::Next
    }

    fn download_app(&mut self, id: StepId) -> Flow {
        if let Some(f) = self.questions() {
            return f;
        }
        let key = if id == StepId::Torrent {
            "torrent"
        } else {
            "usenet"
        };
        let answer = self.answers.get(key).to_owned();
        let Some(implementation) = install::download_client_for(&answer) else {
            return Flow::Next;
        };
        if let Some((intro, list)) = install::download_needs(id, &self.answers) {
            self.say(intro);
            for p in list {
                self.say(&format!("  {}, {} (link to come)", p.name, p.note));
            }
            self.say(PLACEHOLDER_NOTE);
        }
        self.add_provider(ProviderKind::DownloadClient, implementation, id)
    }

    fn sites(&mut self) -> Flow {
        let Some(schemas) = self.call(
            self.rt
                .block_on(self.api.get::<Vec<ProviderSchemaDto>>("/indexers/schema")),
        ) else {
            return Flow::Skip;
        };
        loop {
            let added = self
                .call(self.rt.block_on(self.api.get::<Vec<ProviderDto>>("/indexers")))
                .unwrap_or_default();
            if !added.is_empty() {
                let names: Vec<&str> = added.iter().map(|p| p.name.as_str()).collect();
                self.say(&format!("Sites added: {}", names.join(", ")));
            }
            let find = got!(self.text(
                "Find a site to add",
                "Type part of its name, such as nyaa, or torznab for any Torznab address. Press Enter with nothing typed when you have finished.",
                "",
                None,
            ));
            if find.is_empty() {
                return if added.is_empty() { Flow::Skip } else { Flow::Next };
            }
            let needle = find.to_lowercase();
            let found: Vec<&ProviderSchemaDto> = schemas
                .iter()
                .filter(|s| {
                    s.schema.name.to_lowercase().contains(&needle)
                        || s.schema.implementation.contains(&needle)
                })
                .collect();
            if found.is_empty() {
                self.say(&format!(
                    "No site matches {find}. Try a shorter part of its name."
                ));
                continue;
            }
            // A long list on a short screen shows what fits, then the rest on m.
            let fits = self.ui.rows.saturating_sub(12).clamp(3, 20);
            let mut shown = fits.min(found.len());
            let pick = loop {
                let mut lines = vec![self.ui.bold("Choose the site")];
                for (i, s) in found.iter().take(shown).enumerate() {
                    let privacy = s.privacy.as_deref().unwrap_or("");
                    lines.push(format!(
                        " {:>2}. {} {}",
                        i + 1,
                        s.schema.name,
                        self.ui.dim(privacy)
                    ));
                }
                if shown < found.len() {
                    lines.push(format!("{} more: type m to see them", found.len() - shown));
                }
                let reply = got!(self.ask(&lines, "› Your choice [1]: ", "first", false));
                if reply.trim().eq_ignore_ascii_case("m") {
                    shown = found.len();
                    continue;
                }
                let n = if reply.trim().is_empty() {
                    1
                } else {
                    match parse_numbers(&reply, shown).as_deref() {
                        Some([n]) => *n,
                        _ => continue,
                    }
                };
                break found[n - 1].schema.implementation.clone();
            };
            match self.add_provider_from(&schemas, ProviderKind::Indexer, &pick, StepId::Sites) {
                Flow::Back => return Flow::Back,
                Flow::Stop => return Flow::Stop,
                _ => {}
            }
        }
    }

    fn media_servers(&mut self) -> Flow {
        let Some(schemas) = self.call(
            self.rt
                .block_on(self.api.get::<Vec<ProviderSchemaDto>>("/mediaservers/schema")),
        ) else {
            return Flow::Skip;
        };
        let mut lines = vec![self.ui.bold("Which app do you watch or listen with?")];
        for (i, s) in schemas.iter().enumerate() {
            let rec = if s.schema.implementation == "jellyfin" {
                " ● (recommended)"
            } else {
                ""
            };
            lines.push(format!(" {:>2}. {}{rec}", i + 1, s.schema.name));
        }
        let none = schemas.len() + 1;
        lines.push(format!(
            " {none:>2}. Not yet: add it later in Settings, then Media servers"
        ));
        let default = schemas
            .iter()
            .position(|s| s.schema.implementation == "jellyfin")
            .map(|i| i + 1)
            .unwrap_or(1);
        let reply = got!(self.ask(
            &lines,
            &format!("› Your choice [{default}]: "),
            "recommended",
            false
        ));
        let n = if reply.trim().is_empty() {
            default
        } else {
            match parse_numbers(&reply, none).as_deref() {
                Some([n]) => *n,
                _ => return self.run_step(StepId::MediaServers),
            }
        };
        if n == none {
            return Flow::Skip;
        }
        let implementation = schemas[n - 1].schema.implementation.clone();
        self.add_provider_from(
            &schemas,
            ProviderKind::MediaServer,
            &implementation,
            StepId::MediaServers,
        )
    }

    fn add_provider(&mut self, kind: ProviderKind, implementation: &str, step: StepId) -> Flow {
        let Some(schemas) = self.call(
            self.rt.block_on(
                self.api
                    .get::<Vec<ProviderSchemaDto>>(&format!("/{}/schema", kind.path())),
            ),
        ) else {
            return Flow::Skip;
        };
        self.add_provider_from(&schemas, kind, implementation, step)
    }

    /// Asks each field Reelix's own form asks for `implementation` (not the advanced ones), tests
    /// the connection, then saves it.
    fn add_provider_from(
        &mut self,
        schemas: &[ProviderSchemaDto],
        kind: ProviderKind,
        implementation: &str,
        step: StepId,
    ) -> Flow {
        let Some(schema) = schemas.iter().find(|s| s.schema.implementation == implementation) else {
            return Flow::Skip;
        };
        let schema = schema.schema.clone();
        let mut settings = Map::new();
        for f in schema.fields.iter().filter(|f| !f.advanced) {
            let default = f.default.as_ref().map(value_text).unwrap_or_default();
            let mut hint = f.help.clone().unwrap_or_default();
            if !f.required {
                hint = format!("{hint} Press Enter to leave it as it is.")
                    .trim()
                    .to_owned();
            }
            let value = match &f.kind {
                FieldKind::Bool => {
                    let lines = vec![
                        self.ui.bold(&f.label),
                        "  1. Yes".to_owned(),
                        "  2. No".to_owned(),
                    ];
                    let d = if default == "true" { "1" } else { "2" };
                    let reply = got!(self.ask(&lines, &format!("› Your choice [{d}]: "), "keep", false));
                    let yes = if reply.trim().is_empty() {
                        d == "1"
                    } else {
                        reply.trim() == "1"
                    };
                    Value::Bool(yes)
                }
                FieldKind::Select { options } => {
                    let mut lines = vec![self.ui.bold(&f.label)];
                    for (i, o) in options.iter().enumerate() {
                        lines.push(format!(" {:>2}. {}", i + 1, o.label));
                    }
                    let d = options
                        .iter()
                        .position(|o| Some(&o.value) == f.default.as_ref())
                        .map(|i| i + 1)
                        .unwrap_or(1);
                    let reply = got!(self.ask(&lines, &format!("› Your choice [{d}]: "), "keep", false));
                    let n = parse_numbers(&reply, options.len())
                        .and_then(|v| v.first().copied())
                        .unwrap_or(d);
                    options[n - 1].value.clone()
                }
                FieldKind::MultiSelect { .. } | FieldKind::List => continue,
                FieldKind::Secret => {
                    let v = got!(self.text(&f.label, &hint, "", Some("provider-secret")));
                    self.secrets.remove("provider-secret");
                    if v.is_empty() {
                        continue;
                    }
                    Value::String(v)
                }
                FieldKind::Number => {
                    let v = got!(self.text(&f.label, &hint, &default, None));
                    match v.parse::<i64>() {
                        Ok(n) => Value::from(n),
                        Err(_) if v.is_empty() => continue,
                        Err(_) => Value::String(v),
                    }
                }
                FieldKind::Text | FieldKind::Url | FieldKind::Path => {
                    let v = got!(self.text(&f.label, &hint, &default, None));
                    if v.is_empty() {
                        continue;
                    }
                    Value::String(v)
                }
            };
            settings.insert(f.name.clone(), value);
        }
        let input = ProviderInput {
            implementation: implementation.to_owned(),
            name: schema.name.clone(),
            enabled: true,
            priority: 25,
            tags: Vec::new(),
            settings: Value::Object(settings),
        };
        let test = ProviderTestRequest {
            id: None,
            provider: input.clone(),
        };
        let path = format!("/{}", kind.path());
        match self.call(
            self.rt
                .block_on(self.api.post::<_, TestResultDto>(&format!("{path}/test"), &test)),
        ) {
            Some(r) if r.ok => {}
            Some(r) => {
                let why = r.message.unwrap_or_else(|| "the connection test failed".into());
                self.last_error = Some(format!("{} did not answer: {why}", schema.name));
                return self.run_step(step);
            }
            None => return self.run_step(step),
        }
        if self
            .call(self.rt.block_on(self.api.post::<_, ProviderDto>(&path, &input)))
            .is_none()
        {
            return self.run_step(step);
        }
        self.say(&format!("{} added and tested.", term::clean(&schema.name)));
        Flow::Next
    }

    /// Saves the answers of the steps that are only questions. `false` keeps the person on it.
    fn save_answers(&mut self, id: StepId) -> bool {
        let a = self.answers.clone();
        let api = &self.api;
        let res = self.rt.block_on(async {
            match id {
                StepId::DownloadOptions => {
                    let prefer = if a.get("prefer") == "torrent" {
                        Protocol::Torrent
                    } else {
                        Protocol::Usenet
                    };
                    let profiles = api.get::<Vec<DelayProfile>>("/delayprofiles").await?;
                    if let Some(mut p) = profiles.into_iter().find(|p| p.tags.is_empty()) {
                        p.preferred_protocol = prefer;
                        api.put::<_, DelayProfile>(&format!("/delayprofiles/{}", p.id), &p)
                            .await?;
                    }
                }
                StepId::Quality => {
                    let name = install::quality_profile_for(a.get("quality"));
                    let profiles = api.get::<Vec<QualityProfile>>("/qualityprofiles").await?;
                    let pid = profiles.iter().find(|p| p.name == name).map(|p| p.id);
                    let mut r = api.get::<RequestSettingsDto>("/settings/requests").await?;
                    r.movie.profile_id = pid;
                    r.series.profile_id = pid;
                    api.put::<_, RequestSettingsDto>("/settings/requests", &r).await?;
                }
                StepId::Monitor => {
                    let mut r = api.get::<RequestSettingsDto>("/settings/requests").await?;
                    r.minimum_availability =
                        install::minimum_availability_for(a.get("movie_when")).to_owned();
                    api.put::<_, RequestSettingsDto>("/settings/requests", &r).await?;
                }
                StepId::Requests => {
                    let auto = a.get("requests") == "auto";
                    let mut r = api.get::<RequestSettingsDto>("/settings/requests").await?;
                    r.auto_approve_movie = auto;
                    r.auto_approve_series = auto;
                    api.put::<_, RequestSettingsDto>("/settings/requests", &r).await?;
                }
                _ => {}
            }
            Ok::<_, api::ApiError>(())
        });
        match res {
            Ok(()) => true,
            Err(e) => {
                self.last_error = Some(format!("Reelix could not save this step: {}", e.message));
                false
            }
        }
    }

    fn finish(&mut self) {
        let restore = self.answers.start() == Start::Restore;
        let mut lines = Vec::new();
        if restore {
            lines.push("The backup is ready. Restart Reelix to finish restoring:".to_owned());
            lines.push("  docker restart reelix".to_owned());
            lines.push("Then sign in with an account from the backup.".to_owned());
        } else {
            for id in self.shown() {
                if matches!(id, StepId::Start | StepId::Code | StepId::Finish) {
                    continue;
                }
                let state = if self.skipped.contains(&id) {
                    "skipped"
                } else {
                    "set"
                };
                lines.push(format!("{}: {state}", install::step(id).title));
            }
        }
        lines.push(String::new());
        lines.push(format!(
            "Open Reelix on this computer: http://localhost:{}",
            self.port
        ));
        lines.push(format!(
            "Open it from other devices:   http://{}:{}",
            local_address(),
            self.port
        ));
        lines.push(String::new());
        lines.push("To ask these questions again: reelix setup".to_owned());
        lines.push("(with Docker: docker exec -it reelix /app/reelix setup)".to_owned());
        if self.ui.plain {
            println!();
            println!("Reelix, done.");
            for l in &lines {
                self.print_plain(l);
            }
            return;
        }
        self.ui.refresh();
        self.ui.quiet();
        let pad = " ".repeat(self.ui.left());
        let mut out = String::from("\x1b[2J\x1b[H");
        let head = Head {
            title: "Done".into(),
            n: 1,
            total: 1,
        };
        for l in self.ui.heading(&head).iter().take(3) {
            out.push_str(&pad);
            out.push_str(l);
            out.push_str("\r\n");
        }
        for l in self.ui.card("Reelix is ready", &lines) {
            out.push_str(&pad);
            out.push_str(&l);
            out.push_str("\r\n");
        }
        print!("{out}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
}

const PLAIN_HELP: Help = Help {
    term: "Plain mode for screen readers",
    text: &[
        "Run reelix setup --plain, or set REELIX_PLAIN=1. It asks the same questions as lines that scroll, with nothing redrawn.",
    ],
};

/// This server's address on the home network, as other devices reach it. In a container this is
/// the container's address, so it says so instead.
fn local_address() -> String {
    if std::path::Path::new("/.dockerenv").exists() {
        return "your-server-address".into();
    }
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            // No packet is sent: this only asks which address the system would use.
            s.connect("192.0.2.1:9")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "your-server-address".into())
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// "1,3" or "2" as numbers from 1 to `max`, in the order typed, without repeats.
fn parse_numbers(s: &str, max: usize) -> Option<Vec<usize>> {
    let mut out = Vec::new();
    for part in s.split([',', ' ']).filter(|p| !p.is_empty()) {
        let n: usize = part.parse().ok()?;
        if n == 0 || n > max {
            return None;
        }
        if !out.contains(&n) {
            out.push(n);
        }
    }
    Some(out)
}

/// How the folder lists name a books or music folder's kind.
fn content_key(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Ebook => "ebook",
        ContentKind::Audiobook => "audiobook",
        ContentKind::Music => "music",
        ContentKind::Movie => "movie",
        ContentKind::Series => "series",
        ContentKind::Radio => "radio",
    }
}

/// The largest backup Reelix restores (the server refuses more).
const MAX_BACKUP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Where the wizard keeps an app's API key while that app is being brought over: one place per
/// app, so one app's key is never offered for another.
fn arr_key_slot(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Radarr => "arr-key-radarr",
        SourceKind::Sonarr => "arr-key-sonarr",
        SourceKind::Prowlarr => "arr-key-prowlarr",
        SourceKind::Readarr => "arr-key-readarr",
        SourceKind::Lidarr => "arr-key-lidarr",
        SourceKind::Jellyseerr => "arr-key-jellyseerr",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn each_app_keeps_its_own_api_key() {
        let slots: std::collections::HashSet<_> = super::SourceKind::ALL
            .iter()
            .map(|s| super::arr_key_slot(*s))
            .collect();
        assert_eq!(slots.len(), super::SourceKind::ALL.len());
    }

    #[test]
    fn plain_http_to_another_computer_is_noticed() {
        assert!(super::plain_http_elsewhere("http://192.168.1.20:7700"));
        assert!(super::plain_http_elsewhere("http://server.lan:7700/reelix"));
        assert!(!super::plain_http_elsewhere("http://127.0.0.1:7700"));
        assert!(!super::plain_http_elsewhere("http://localhost:7700"));
        assert!(!super::plain_http_elsewhere("http://[::1]:7700"));
        assert!(!super::plain_http_elsewhere("https://server.lan"));
    }

    use super::*;

    #[test]
    fn numbers_are_read_as_typed() {
        assert_eq!(parse_numbers("1,3", 4), Some(vec![1, 3]));
        assert_eq!(parse_numbers("2, 2", 4), Some(vec![2]));
        assert_eq!(parse_numbers("5", 4), None);
        assert_eq!(parse_numbers("x", 4), None);
    }
}
