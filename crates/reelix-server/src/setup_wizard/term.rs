//! Drawing the terminal wizard: boxed (a cleared screen per step, a heading box, a progress bar
//! and a menu on the bottom line) or plain (numbered lines that scroll, nothing redrawn, for
//! screen readers and small or non-terminal outputs).
//!
//! The boxed look sits in a column at most 100 characters wide, centred in a wider window. Text
//! is kept as paragraphs and wrapped between words only when it is drawn, so when the window
//! changes size the step is drawn again, wrapped to the new width.

use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// The widest the column gets, so lines stay readable on a wide window.
pub const MAX_WIDTH: usize = 100;
/// Below this size the boxed drawing does not fit, so plain mode is used.
pub const MIN_WIDTH: usize = 60;
pub const MIN_HEIGHT: usize = 20;
/// The keys on the menu line while a question waits.
pub const STEP_KEYS: &str = "b Back   ? Help   f Feedback   q Stop";
/// The line under a list with marks.
pub const PICK_NOTE: &str = "Up and Down move, Space ticks or unticks, Enter carries on.";

#[derive(Debug, Clone)]
pub struct Ui {
    pub plain: bool,
    pub color: bool,
    /// The window's width and height.
    pub cols: usize,
    pub rows: usize,
    /// What is on the screen now, shared with the thread that watches the window's size.
    live: Arc<Mutex<Live>>,
}

/// The heading box: the step's name and where it is in the workflow.
#[derive(Debug, Clone)]
pub struct Head {
    pub title: String,
    pub n: usize,
    pub total: usize,
}

/// A whole step on the screen, kept so it can be drawn again at a new size.
#[derive(Debug, Clone)]
pub struct Screen {
    pub head: Head,
    /// Paragraphs, wrapped to the column when they are drawn.
    pub body: Vec<String>,
    /// A list with marks under the body, for questions with several answers.
    pub pick: Option<Pick>,
    /// The last line: the waiting question, with the cursor after it.
    pub prompt: String,
    /// The prompt is a job's progress line: cut to the column, never wrapped.
    pub cut: bool,
    /// The keys on the menu line, and what Enter does.
    pub keys: String,
    pub enter: String,
}

/// A list where several answers can be ticked.
#[derive(Debug, Clone)]
pub struct Pick {
    /// Each answer's name, and the line about it.
    pub items: Vec<(String, String)>,
    pub ticked: Vec<bool>,
    /// The line the cursor is on.
    pub at: usize,
    /// The line under the list: the keys, or what to do instead.
    pub note: String,
}

/// What was drawn last, and at which window size.
#[derive(Debug, Default)]
struct Live {
    screen: Option<Screen>,
    size: (usize, usize),
    /// How many lines the list with marks took, to draw it again in place.
    pick_len: usize,
}

impl Ui {
    pub fn new(plain: bool, color: bool, cols: usize, rows: usize) -> Self {
        Self {
            plain,
            color,
            cols,
            rows,
            live: Arc::default(),
        }
    }

    /// Boxed when the output is a terminal of at least 60 by 20 that can draw; plain otherwise,
    /// or when asked (`--plain`, `REELIX_PLAIN=1`).
    pub fn detect(force_plain: bool) -> Self {
        let (cols, rows) = window_size();
        let tty = io::stdout().is_terminal() && io::stdin().is_terminal();
        let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
        let plain = force_plain || !tty || dumb || cols < MIN_WIDTH || rows < MIN_HEIGHT;
        let color = std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty());
        let ui = Self::new(plain, color, cols, rows);
        if !plain {
            ui.watch_size();
        }
        ui
    }

    /// Reads the window size again (it can change between steps).
    pub fn refresh(&mut self) {
        let (cols, rows) = window_size();
        self.cols = cols;
        self.rows = rows;
    }

    /// The column's width: boxes, menus and wrapped text. Plain lines use the whole window.
    pub fn width(&self) -> usize {
        if self.plain {
            self.cols.saturating_sub(1).max(20)
        } else {
            self.cols.clamp(MIN_WIDTH, MAX_WIDTH)
        }
    }

    /// The space left of the column, so it is centred in a window wider than 100 characters.
    pub fn left(&self) -> usize {
        if self.plain {
            0
        } else {
            self.cols.saturating_sub(self.width()) / 2
        }
    }

    fn margin(&self) -> String {
        " ".repeat(self.left())
    }

    fn sgr(&self, code: &str, s: &str) -> String {
        if self.color && !self.plain {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    }

    pub fn bold(&self, s: &str) -> String {
        self.sgr("1", s)
    }

    pub fn dim(&self, s: &str) -> String {
        self.sgr("2", s)
    }

    pub fn accent(&self, s: &str) -> String {
        self.sgr("1;35", s)
    }

    pub fn danger(&self, s: &str) -> String {
        self.sgr("1;31", s)
    }

    /// The heading box and progress bar (boxed), or the heading line (plain).
    pub fn heading(&self, head: &Head) -> Vec<String> {
        let pct = head.n.saturating_sub(1) * 100 / head.total.max(1);
        if self.plain {
            return vec![format!(
                "Reelix, step {} of {}: {} ({pct}% done)",
                head.n, head.total, head.title
            )];
        }
        let w = self.width();
        let inner = w - 4;
        let right = format!("step {} of {}", head.n, head.total);
        let room = inner.saturating_sub(right.chars().count() + 1);
        let title = clip(&head.title, room.saturating_sub("Reelix | ".len()));
        let left = format!("{} {} {}", self.bold("Reelix"), self.dim("|"), title);
        let left_len = "Reelix | ".len() + title.chars().count();
        let gap = inner.saturating_sub(left_len + right.chars().count());
        let bar_w = w - 5;
        let filled = bar_w * pct / 100;
        vec![
            format!("┌{}┐", "─".repeat(w - 2)),
            format!("│ {left}{}{} │", " ".repeat(gap), self.dim(&right)),
            format!("└{}┘", "─".repeat(w - 2)),
            format!(
                "{}{} {:>3}%",
                self.accent(&"█".repeat(filled)),
                self.dim(&"░".repeat(bar_w - filled)),
                pct
            ),
        ]
    }

    /// A box with a title and lines inside, at the column's width.
    pub fn card(&self, title: &str, lines: &[String]) -> Vec<String> {
        let w = self.width();
        let inner = w - 4;
        let mut out = vec![format!(
            "┌─ {} {}┐",
            self.bold(title),
            "─".repeat(w.saturating_sub(5 + title.chars().count()))
        )];
        for l in lines {
            for part in wrap(l, inner) {
                let pad = inner.saturating_sub(visible_len(&part));
                out.push(format!("│ {part}{} │", " ".repeat(pad)));
            }
        }
        out.push(format!("└{}┘", "─".repeat(w - 2)));
        out
    }

    /// The menu on the bottom line: the keys that work here, and what Enter does on the right.
    pub fn menu_line(&self, keys: &str, enter: &str) -> String {
        let w = self.width();
        let right = if enter.is_empty() {
            String::new()
        } else {
            format!("Enter: {enter}")
        };
        let gap = w.saturating_sub(keys.chars().count() + right.chars().count());
        self.sgr("7", &format!("{keys}{}{right}", " ".repeat(gap)))
    }

    /// The lines of a list with marks: `›` on the cursor's line, `[x]` on the ticked ones, then
    /// the note. Every line fits the column.
    pub fn pick_lines(&self, pick: &Pick) -> Vec<String> {
        let w = self.width();
        // "  › [x]  1. ": the cursor, the mark and the number, then the name.
        let hang = 12;
        let room = w.saturating_sub(hang).max(10);
        let mut out = Vec::new();
        for (i, (name, about)) in pick.items.iter().enumerate() {
            let cursor = if i == pick.at {
                self.accent("›")
            } else {
                " ".to_owned()
            };
            let mark = if pick.ticked.get(i).copied().unwrap_or(false) {
                self.accent("[x]")
            } else {
                "[ ]".to_owned()
            };
            for (j, line) in wrap(name, room).iter().enumerate() {
                if j == 0 {
                    out.push(format!("  {cursor} {mark} {:>2}. {line}", i + 1));
                } else {
                    out.push(format!("{}{line}", " ".repeat(hang)));
                }
            }
            if !about.is_empty() {
                for line in wrap(about, room) {
                    out.push(format!("{}{}", " ".repeat(hang), self.dim(&line)));
                }
            }
        }
        out.push(String::new());
        // Wrapped, not cut: at 60 columns the usual hint is one character too long.
        for line in wrap(&pick.note, w - 2) {
            out.push(format!("  {}", self.dim(&line)));
        }
        out
    }

    /// The whole step as text to print: clear, the heading, the step's last lines, the menu on
    /// the bottom line, and the cursor after the prompt. Everything is in the column, with the
    /// margin added as it is printed.
    pub fn render(&self, s: &Screen) -> String {
        let w = self.width();
        let pad = self.margin();
        let top = self.heading(&s.head);
        let mut lines: Vec<String> = s.body.iter().flat_map(|p| wrap(p, w)).collect();
        if let Some(p) = &s.pick {
            lines.extend(self.pick_lines(p));
        }
        let room = self.rows.saturating_sub(top.len() + 3);
        // Keep the end of the step (the question) in view on a short window.
        let start = lines.len().saturating_sub(room);
        let mut out = String::from("\x1b[2J\x1b[H");
        for line in top.iter().chain(&lines[start..]) {
            out.push_str(&pad);
            out.push_str(line);
            out.push_str("\r\n");
        }
        out.push_str(&format!(
            "\x1b7\x1b[{};{}H{}\x1b8",
            self.rows,
            self.left() + 1,
            self.menu_line(&s.keys, &s.enter)
        ));
        out.push_str(&pad);
        if s.cut {
            out.push_str(&clip_visible(&s.prompt, w));
        } else {
            out.push_str(&s.prompt);
        }
        out
    }

    /// Draws a whole step and keeps it, so a change of window size draws it again. Plain mode
    /// never calls this.
    pub fn show(&self, screen: Screen) {
        let mut live = self.lock();
        self.sized().draw_live(&mut live, screen);
    }

    /// Draws the list with marks again in place (the cursor moves up over it and writes it
    /// again), so it never repeats down the window.
    pub fn update_pick(&self, pick: Pick) {
        let mut live = self.lock();
        let ui = self.sized();
        let Some(mut screen) = live.screen.clone() else {
            return;
        };
        screen.pick = Some(pick.clone());
        let lines = ui.pick_lines(&pick);
        if live.size != (ui.cols, ui.rows) || live.pick_len != lines.len() || live.pick_len == 0 {
            ui.draw_live(&mut live, screen);
            return;
        }
        let pad = ui.margin();
        let mut out = format!("\x1b[{}A\r", lines.len());
        for line in &lines {
            out.push_str(&format!("\x1b[2K{pad}{line}\r\n"));
        }
        out.push_str(&pad);
        print!("{out}");
        let _ = io::stdout().flush();
        live.screen = Some(screen);
    }

    /// Writes the prompt line again in place: a job's progress, cut to the column.
    pub fn update_prompt(&self, prompt: String) {
        let mut live = self.lock();
        let ui = self.sized();
        let Some(mut screen) = live.screen.clone() else {
            return;
        };
        screen.prompt = prompt;
        if live.size != (ui.cols, ui.rows) {
            ui.draw_live(&mut live, screen);
            return;
        }
        let line = if screen.cut {
            clip_visible(&screen.prompt, ui.width())
        } else {
            screen.prompt.clone()
        };
        print!("\r\x1b[2K{}{line}", ui.margin());
        let _ = io::stdout().flush();
        live.screen = Some(screen);
    }

    /// Forgets the step on the screen, before something else is drawn over it (help, feedback,
    /// the QR code, the end), so a change of size does not draw the step over it.
    pub fn quiet(&self) {
        self.lock().screen = None;
    }

    fn lock(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// This drawing at the window's size now.
    fn sized(&self) -> Ui {
        let (cols, rows) = window_size();
        Ui {
            cols,
            rows,
            ..self.clone()
        }
    }

    fn draw_live(&self, live: &mut Live, screen: Screen) {
        print!("{}", self.render(&screen));
        let _ = io::stdout().flush();
        live.size = (self.cols, self.rows);
        live.pick_len = screen.pick.as_ref().map_or(0, |p| self.pick_lines(p).len());
        live.screen = Some(screen);
    }

    /// Draws the step again when the window changes size: the column is worked out again, the
    /// text wrapped again, and the waiting question moves with it. A small thread looks at the
    /// size four times a second (a read from the terminal, no work otherwise).
    fn watch_size(&self) {
        let ui = self.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(250));
                let (cols, rows) = window_size();
                let mut live = ui.lock();
                if live.size == (cols, rows) || cols < MIN_WIDTH || rows < MIN_HEIGHT {
                    continue;
                }
                if let Some(screen) = live.screen.clone() {
                    Ui {
                        cols,
                        rows,
                        ..ui.clone()
                    }
                    .draw_live(&mut live, screen);
                }
            }
        });
    }

    /// The width of the box [`Ui::overlay`] draws.
    pub fn overlay_width(&self) -> usize {
        self.width().min(self.cols.saturating_sub(4)).max(40)
    }

    /// Draws `behind` dimmed with a box in the middle (help, or the feedback steps).
    pub fn overlay(&self, behind: &[String], title: &str, text: &[String], menu: &str) {
        self.quiet();
        let w = self.overlay_width();
        let inner = w - 4;
        let mut boxed = vec![format!(
            "┌─ {} {}┐",
            self.bold(title),
            "─".repeat(w.saturating_sub(5 + title.chars().count()))
        )];
        for l in text {
            for part in wrap(l, inner) {
                let pad = inner.saturating_sub(visible_len(&part));
                boxed.push(format!("│ {part}{} │", " ".repeat(pad)));
            }
        }
        boxed.push(format!("└{}┘", "─".repeat(w - 2)));
        let pad = self.margin();
        let mut out = String::from("\x1b[2J\x1b[H");
        let behind: Vec<String> = behind.iter().flat_map(|l| wrap(l, self.width())).collect();
        for line in behind.iter().take(self.rows.saturating_sub(1)) {
            out.push_str(&pad);
            out.push_str(&self.dim(&strip_ansi(line)));
            out.push_str("\r\n");
        }
        let top = self.rows.saturating_sub(boxed.len() + 1) / 2 + 1;
        let left = (self.cols.saturating_sub(w)) / 2 + 1;
        for (i, line) in boxed.iter().enumerate() {
            out.push_str(&format!("\x1b[{};{left}H{line}", top + i));
        }
        out.push_str(&format!("\x1b[{};1H\x1b[2K{pad}{menu}", self.rows));
        print!("{out}");
        let _ = io::stdout().flush();
    }

    /// Leaves the boxed drawing: clears the menu line and puts the cursor under the last line.
    pub fn release(&self) {
        self.quiet();
        if !self.plain {
            print!("\x1b[{};1H\x1b[2K\r\n", self.rows);
            let _ = io::stdout().flush();
        }
    }

    /// Whether keys can be read one at a time for a list with marks: the boxed look in a real
    /// terminal. Plain mode keeps typed numbers, which suits software that reads text aloud.
    pub fn keys_work(&self) -> bool {
        cfg!(unix) && !self.plain && io::stdin().is_terminal() && io::stdout().is_terminal()
    }
}

/// A key pressed while a list with marks is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Space,
    Enter,
    Char(char),
    /// Ctrl+C or Ctrl+D, or the input ended.
    Stop,
    Other,
}

/// The keys in what one read gave, in order (a fast typist can send several at once).
pub fn parse_keys(bytes: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        let key = match b {
            0x1b if matches!(bytes.get(i), Some(b'[' | b'O')) => {
                let end = bytes[i + 1..]
                    .iter()
                    .position(|c| (0x40..=0x7e).contains(c))
                    .map_or(bytes.len(), |p| i + 1 + p);
                let key = match bytes.get(end) {
                    Some(b'A') if end == i + 1 => Key::Up,
                    Some(b'B') if end == i + 1 => Key::Down,
                    _ => Key::Other,
                };
                i = end + 1;
                key
            }
            0x03 | 0x04 => Key::Stop,
            b'\r' | b'\n' => Key::Enter,
            b' ' => Key::Space,
            b if b.is_ascii_graphic() => Key::Char(b as char),
            _ => Key::Other,
        };
        keys.push(key);
    }
    keys
}

/// Keys one at a time and not shown, for the list with marks. Dropping it puts the terminal
/// back as it was, also after an error or a panic; Ctrl+C arrives as a key and stops the
/// wizard the usual way, so the terminal is never left like this.
#[cfg(unix)]
pub struct Keys {
    saved: rustix::termios::Termios,
}

#[cfg(unix)]
impl Keys {
    pub fn start() -> io::Result<Self> {
        use rustix::termios::{LocalModes, OptionalActions, SpecialCodeIndex, tcgetattr, tcsetattr};
        let stdin = io::stdin();
        let saved = tcgetattr(&stdin)?;
        let mut keys = saved.clone();
        keys.local_modes
            .remove(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ISIG);
        keys.special_codes[SpecialCodeIndex::VMIN] = 1;
        keys.special_codes[SpecialCodeIndex::VTIME] = 0;
        tcsetattr(&stdin, OptionalActions::Drain, &keys)?;
        // No cursor on the screen while the list is up: the `›` mark shows where it is.
        print!("\x1b[?25l");
        let _ = io::stdout().flush();
        Ok(Self { saved })
    }

    /// Waits for the next keys.
    pub fn read(&self) -> Vec<Key> {
        let mut buf = [0u8; 16];
        match io::stdin().read(&mut buf) {
            Ok(0) | Err(_) => vec![Key::Stop],
            Ok(n) => parse_keys(&buf[..n]),
        }
    }
}

#[cfg(unix)]
impl Drop for Keys {
    fn drop(&mut self) {
        let _ = rustix::termios::tcsetattr(io::stdin(), rustix::termios::OptionalActions::Drain, &self.saved);
        print!("\x1b[?25h");
        let _ = io::stdout().flush();
    }
}

/// Keys typed while a progress line turns are not shown, so Enter cannot push the line down the
/// window, and they are thrown away when it ends. Ctrl+C still stops the wizard. Dropping it puts
/// the terminal back as it was.
#[cfg(unix)]
pub struct Quiet {
    saved: rustix::termios::Termios,
}

#[cfg(unix)]
impl Quiet {
    pub fn start() -> io::Result<Self> {
        use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};
        let stdin = io::stdin();
        let saved = tcgetattr(&stdin)?;
        let mut quiet = saved.clone();
        quiet.local_modes.remove(LocalModes::ICANON | LocalModes::ECHO);
        tcsetattr(&stdin, OptionalActions::Drain, &quiet)?;
        Ok(Self { saved })
    }
}

#[cfg(unix)]
impl Drop for Quiet {
    fn drop(&mut self) {
        use rustix::termios::{OptionalActions, QueueSelector, tcflush, tcsetattr};
        let _ = tcflush(io::stdin(), QueueSelector::IFlush);
        let _ = tcsetattr(io::stdin(), OptionalActions::Drain, &self.saved);
    }
}

#[cfg(not(unix))]
pub struct Quiet;

#[cfg(not(unix))]
impl Quiet {
    pub fn start() -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Without termios the list with marks is not offered ([`Ui::keys_work`]), so this never runs.
#[cfg(not(unix))]
pub struct Keys;

#[cfg(not(unix))]
impl Keys {
    pub fn start() -> io::Result<Self> {
        Err(io::ErrorKind::Unsupported.into())
    }

    pub fn read(&self) -> Vec<Key> {
        vec![Key::Stop]
    }
}

/// Reads one line. `None` when input has ended (Ctrl+D, or a closed pipe).
pub fn read_line() -> Option<String> {
    let mut s = String::new();
    match io::stdin().lock().read_line(&mut s) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(s.trim_end_matches(['\r', '\n']).to_owned()),
    }
}

/// Reads a line without showing it. Falls back to a plain read when there is no terminal.
pub fn read_secret() -> Option<String> {
    let _ = io::stdout().flush();
    if io::stdin().is_terminal() {
        rpassword::read_password().ok()
    } else {
        read_line()
    }
}

fn window_size() -> (usize, usize) {
    if let Some((terminal_size::Width(w), terminal_size::Height(h))) = terminal_size::terminal_size() {
        return (w as usize, h as usize);
    }
    let env = |k: &str, d: usize| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    (env("COLUMNS", 80), env("LINES", 24))
}

/// Characters on screen, without colour codes.
pub fn visible_len(s: &str) -> usize {
    strip_ansi(s).chars().count()
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Cuts `s` to `max` characters, ending with "…" when it was cut.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Cuts `s` to `max` characters on screen, keeping its colour codes, so a progress line never
/// runs onto a second line.
pub fn clip_visible(s: &str, max: usize) -> String {
    if visible_len(s) <= max {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut shown = 0;
    let mut coloured = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            coloured = true;
            out.push(c);
            for c in chars.by_ref() {
                out.push(c);
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if shown + 1 < max {
            out.push(c);
            shown += 1;
        } else {
            break;
        }
    }
    out.push('…');
    if coloured {
        out.push_str("\x1b[0m");
    }
    out
}

/// Text from a server, without control characters (escape sequences among them), so it cannot
/// redraw the screen or fake a prompt. Line breaks and tabs stay.
pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// Wraps `text` between words to `width` characters on screen, never cutting a word (a word
/// longer than the line, such as a link, has a line of its own). A line break starts a new
/// line. Colour carries on to the next line; an indent, and a number such as " 2. ● ", hang
/// over the lines after the first.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    text.split('\n').flat_map(|p| wrap_line(p, width)).collect()
}

fn wrap_line(text: &str, width: usize) -> Vec<String> {
    if visible_len(text) <= width {
        return vec![text.to_owned()];
    }
    let codes_end = leading_codes(text);
    let (codes, rest) = text.split_at(codes_end);
    let (prefix, words) = rest.split_at(list_prefix(rest));
    let mut hang = prefix.chars().count();
    if hang * 2 > width {
        hang = 0;
    }
    let mut active = codes.to_owned();
    let mut lines = Vec::new();
    let mut line = format!("{codes}{prefix}");
    let mut len = prefix.chars().count();
    let mut empty = true;
    for word in words.split(' ').filter(|w| !w.is_empty()) {
        let wl = visible_len(word);
        if !empty && len + 1 + wl > width {
            if !active.is_empty() {
                line.push_str("\x1b[0m");
            }
            let next = format!("{}{active}", " ".repeat(hang));
            lines.push(std::mem::replace(&mut line, next));
            len = hang;
            empty = true;
        }
        if !empty {
            line.push(' ');
            len += 1;
        }
        line.push_str(word);
        len += wl;
        empty = false;
        follow_codes(&mut active, word);
    }
    if !empty || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// Where the colour codes at the start of `s` end.
fn leading_codes(s: &str) -> usize {
    let mut end = 0;
    while s[end..].starts_with('\x1b') {
        match s[end..].find(|c: char| c.is_ascii_alphabetic()) {
            Some(p) => end += p + 1,
            None => break,
        }
    }
    end
}

/// The colour in force after `word`: a reset clears it, other codes add to it.
fn follow_codes(active: &mut String, word: &str) {
    let mut rest = word;
    while let Some(start) = rest.find('\x1b') {
        let Some(len) = rest[start..].find(|c: char| c.is_ascii_alphabetic()) else {
            break;
        };
        let code = &rest[start..=start + len];
        if code == "\x1b[0m" || code == "\x1b[m" {
            active.clear();
        } else if code.ends_with('m') {
            active.push_str(code);
        }
        rest = &rest[start + len + 1..];
    }
}

/// The indent at the start of a line, with a number such as "12. " and a "● " after it: the
/// lines after the first line up under the words.
fn list_prefix(s: &str) -> usize {
    let spaces = s.len() - s.trim_start_matches(' ').len();
    let after = &s[spaces..];
    let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 || !after[digits..].starts_with(". ") {
        return spaces;
    }
    let mut end = spaces + digits + 1;
    end += s[end..].len() - s[end..].trim_start_matches(' ').len();
    if let Some(rest) = s[end..].strip_prefix("● ") {
        end = s.len() - rest.trim_start_matches(' ').len();
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui(cols: usize) -> Ui {
        Ui::new(false, false, cols, 30)
    }

    fn head() -> Head {
        Head {
            title: "What to follow".into(),
            n: 4,
            total: 14,
        }
    }

    fn words(lines: &[String]) -> Vec<String> {
        lines
            .iter()
            .flat_map(|l| {
                strip_ansi(l)
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn every_boxed_line_ends_at_the_same_column() {
        let u = ui(120);
        let head = u.heading(&Head {
            title: "Movie and series information".into(),
            n: 5,
            total: 14,
        });
        let card = u.card(
            "Done",
            &["Open Reelix on this computer: http://localhost:7700".into()],
        );
        for line in head.iter().take(3).chain(&card) {
            assert_eq!(visible_len(line), MAX_WIDTH, "{line}");
        }
        assert!(head[1].contains("Reelix | Movie and series information"));
        assert!(head[1].trim_end_matches(" │").ends_with("step 5 of 14"));
    }

    #[test]
    fn the_column_is_centred_in_a_wide_window() {
        // At most 100 wide, with equal space either side; from 60 to 100 it is the whole window.
        for (cols, width, left) in [
            (200, 100, 50),
            (160, 100, 30),
            (101, 100, 0),
            (100, 100, 0),
            (80, 80, 0),
            (60, 60, 0),
        ] {
            let u = ui(cols);
            assert_eq!((u.width(), u.left()), (width, left), "{cols} columns");
            assert!(left + width + left <= cols && cols - (left + width + left) <= 1);
        }
        // Plain lines use the whole window, with no margin.
        let mut u = ui(200);
        u.plain = true;
        assert_eq!((u.width(), u.left()), (199, 0));
    }

    #[test]
    fn a_step_is_drawn_in_the_centred_column() {
        let u = Ui::new(false, false, 200, 50);
        let screen = Screen {
            head: head(),
            body: vec![String::new(), "What do you want Reelix to look after?".into()],
            pick: None,
            prompt: "› Your choice [1]: ".into(),
            cut: false,
            keys: STEP_KEYS.into(),
            enter: "recommended".into(),
        };
        let out = u.render(&screen);
        let margin = " ".repeat(50);
        let drawn: Vec<&str> = out
            .trim_start_matches("\x1b[2J\x1b[H")
            .split("\r\n")
            .take_while(|l| !l.contains("\x1b7"))
            .collect();
        assert_eq!(drawn.len(), 6);
        for line in drawn {
            assert!(line.starts_with(&margin), "{line:?}");
        }
        assert!(out.contains(&format!("{margin}┌{}┐", "─".repeat(98))));
        // The menu is on the bottom line, in the column too, and the question waits after the margin.
        assert!(out.contains("\x1b[50;51H"));
        assert!(out.ends_with(&format!("{margin}› Your choice [1]: ")));
    }

    #[test]
    fn text_is_wrapped_again_after_the_window_changes_size() {
        let para = "Reelix looks after the movies and series you follow, finds them on the sites you add, \
                    and sends them to your download apps. Choose at least one.";
        let screen = Screen {
            head: head(),
            body: vec![para.into()],
            pick: None,
            prompt: String::new(),
            cut: false,
            keys: STEP_KEYS.into(),
            enter: "next".into(),
        };
        let lines_at = |cols: usize| -> Vec<String> {
            let u = Ui::new(false, false, cols, 50);
            let out = u.render(&screen);
            let body: Vec<String> = out
                .split("\r\n")
                .skip(4)
                .take_while(|l| !l.contains("\x1b7"))
                .map(|l| l[u.left()..].to_owned())
                .collect();
            for l in &body {
                assert!(visible_len(l) <= u.width(), "{cols}: {l}");
            }
            body
        };
        let wide = lines_at(200);
        let narrow = lines_at(64);
        assert!(narrow.len() > wide.len());
        // Every word is still whole, in order.
        let expected: Vec<String> = para.split_whitespace().map(str::to_owned).collect();
        assert_eq!(words(&wide), expected);
        assert_eq!(words(&narrow), expected);
    }

    #[test]
    fn wrap_keeps_words_and_indent() {
        let lines = wrap("   one two three four five six", 12);
        assert!(
            lines
                .iter()
                .all(|l| l.starts_with("   ") && l.chars().count() <= 12)
        );
        assert_eq!(lines.join(" ").split_whitespace().count(), 6);
    }

    #[test]
    fn wrap_hangs_under_a_numbered_answer_and_keeps_colour() {
        let lines = wrap(
            "  1. ● Movies and series, from TMDB, with the ones you follow (recommended)",
            30,
        );
        assert!(lines.len() > 1);
        assert!(lines[0].starts_with("  1. ● Movies"));
        for l in &lines[1..] {
            assert!(l.starts_with("       ") && !l[7..].starts_with(' '), "{l:?}");
        }
        // A dim line keeps its colour on every line, and every line ends it.
        let dim = "\x1b[2m       one two three four five six seven eight nine ten\x1b[0m";
        let lines = wrap(dim, 24);
        assert!(lines.len() > 1);
        for l in &lines {
            assert!(
                visible_len(l) <= 24 && l.contains("\x1b[2m") && l.ends_with("\x1b[0m"),
                "{l:?}"
            );
        }
        // A word longer than the line is never cut.
        let link = "https://example.org/a/very/long/link/that/does/not/fit";
        assert!(wrap(&format!("Open {link} now"), 20).contains(&link.to_owned()));
    }

    #[test]
    fn a_progress_line_is_cut_to_one_line() {
        let line = format!(
            "  \x1b[1;35m◐\x1b[0m Bringing over Sonarr, 120 of 400 done. Please wait, this can take some time. {}",
            "\x1b[2m0 min 07 s\x1b[0m"
        );
        let cut = clip_visible(&line, 60);
        assert_eq!(visible_len(&cut), 60);
        assert!(cut.ends_with("…\x1b[0m"));
        assert_eq!(clip_visible("short", 60), "short");
    }

    #[test]
    fn a_list_with_marks_shows_the_cursor_and_the_ticks() {
        let u = ui(80);
        let pick = Pick {
            items: vec![
                ("Movies (recommended)".into(), "From TMDB.".into()),
                ("Series (recommended)".into(), String::new()),
                ("Books".into(), "Through Readarr-style sites. ".repeat(6)),
            ],
            ticked: vec![true, false, true],
            at: 1,
            note: PICK_NOTE.into(),
        };
        let lines = u.pick_lines(&pick);
        assert_eq!(lines[0], "    [x]  1. Movies (recommended)");
        assert_eq!(lines[1], "            From TMDB.");
        assert_eq!(lines[2], "  › [ ]  2. Series (recommended)");
        assert_eq!(lines[3], "    [x]  3. Books");
        assert!(lines[4].starts_with("            Through"));
        assert_eq!(lines[lines.len() - 1], format!("  {PICK_NOTE}"));
        assert!(lines.iter().all(|l| visible_len(l) <= u.width()));
        // At a narrower width the list is the same list, wrapped again.
        let narrow = ui(60).pick_lines(&pick);
        assert!(narrow.len() > lines.len());
        assert!(narrow.iter().all(|l| visible_len(l) <= 60));
    }

    #[test]
    fn keys_are_read_as_pressed() {
        assert_eq!(
            parse_keys(b"\x1b[A\x1bOB j1\r?"),
            vec![
                Key::Up,
                Key::Down,
                Key::Space,
                Key::Char('j'),
                Key::Char('1'),
                Key::Enter,
                Key::Char('?')
            ]
        );
        assert_eq!(parse_keys(b"\x03"), vec![Key::Stop]);
        assert_eq!(parse_keys(b"\x1b[1;5C"), vec![Key::Other]);
    }

    #[test]
    fn plain_heading_has_no_box_characters() {
        let mut u = ui(80);
        u.plain = true;
        let head = u.heading(&Head {
            title: "Quality".into(),
            n: 3,
            total: 4,
        });
        assert_eq!(head, vec!["Reelix, step 3 of 4: Quality (50% done)".to_owned()]);
    }
}
