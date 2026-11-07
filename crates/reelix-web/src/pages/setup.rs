//! `/setup`: the first-run install wizard.
//!
//! The first question picks one of three workflows: a new install, restoring a Reelix backup, or
//! migrating from the *arr apps. Every step, question, answer and help text comes from
//! [`reelix_api_types::install`], which the terminal wizard (`reelix setup`) reads too, so both ask
//! the same questions in the same order. Each answer is saved to the setting Reelix already has.
//! Steps after the account can be skipped and changed later in Settings.
//!
//! Send feedback (Snaglist, loaded on first use from `snaglist/feedback.js`) is always in the
//! header, and each report names the step it came from.

use std::collections::BTreeSet;

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use reelix_api_types::install::{
    self, Answers, Context, HELP_CENTRE, Help, PLACEHOLDER_NOTE, Provider, Question, Start, StepId,
};
use reelix_api_types::providers::{MetadataSettingsDto, ProviderKind, TestResultDto};
use reelix_api_types::requests::RequestSettingsDto;
use reelix_api_types::system::PendingRestoreDto;
use reelix_api_types::{SetupRequest, SetupStatus, UserDto};
use reelix_core::Protocol;
use reelix_decision::{DelayProfile, QualityProfile};
use wasm_bindgen::JsCast;

use crate::api;
use crate::components::provider_form::ProviderListCard;
use crate::components::{Icon, Logo, Modal};
use crate::pages::settings::media_management::RootFoldersCard;
use crate::pages::system::migrate::MigratePage;
use crate::state::{AuthState, Boot, Theme, Toasts};

/// Opens Send feedback for `step`, with Reelix's own error message when a step failed.
fn open_feedback(step: &str, title: &str, error: Option<&str>) -> bool {
    let Some(win) = web_sys::window() else {
        return false;
    };
    let Ok(fb) = js_sys::Reflect::get(&win, &"reelixFeedback".into()) else {
        return false;
    };
    let Ok(open) = js_sys::Reflect::get(&fb, &"open".into()) else {
        return false;
    };
    match open.dyn_into::<js_sys::Function>() {
        Ok(f) => f
            .call3(&fb, &step.into(), &title.into(), &error.unwrap_or("").into())
            .is_ok(),
        Err(_) => false,
    }
}

/// Moves focus to the new step's heading, so keyboard and screen reader users start at the top.
fn focus_heading() {
    request_animation_frame(|| {
        if let Some(el) = document()
            .get_element_by_id("wizard-heading")
            .and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let _ = el.focus();
        }
    });
}

#[derive(Clone, Copy)]
struct Wizard {
    answers: RwSignal<Answers>,
    ctx: RwSignal<Context>,
    current: RwSignal<StepId>,
    skipped: RwSignal<BTreeSet<StepKey>>,
    /// Back stops here: the account (or the restore) changed something that cannot be undone.
    floor: RwSignal<Option<StepId>>,
    error: RwSignal<Option<String>>,
    busy: RwSignal<bool>,
    help: RwSignal<Option<&'static Help>>,
    help_open: RwSignal<bool>,
}

/// `StepId` in a `BTreeSet`, by its position in [`install::STEPS`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct StepKey(usize);

impl StepKey {
    fn of(id: StepId) -> Self {
        Self(install::STEPS.iter().position(|s| s.id == id).unwrap_or(0))
    }
}

impl Wizard {
    fn shown(&self) -> Vec<StepId> {
        install::steps_shown(&self.answers.get(), self.ctx.get())
    }

    fn go(&self, to: StepId) {
        self.error.set(None);
        self.help_open.set(false);
        self.current.set(to);
        focus_heading();
    }

    fn next(&self) {
        let shown = install::steps_shown(&self.answers.get_untracked(), self.ctx.get_untracked());
        let cur = self.current.get_untracked();
        if let Some(i) = shown.iter().position(|s| *s == cur)
            && let Some(n) = shown.get(i + 1)
        {
            self.go(*n);
        }
    }

    fn skip(&self) {
        let cur = self.current.get_untracked();
        self.skipped.update(|s| {
            s.insert(StepKey::of(cur));
        });
        self.next();
    }

    fn done(&self) {
        let cur = self.current.get_untracked();
        self.skipped.update(|s| {
            s.remove(&StepKey::of(cur));
        });
        self.next();
    }

    fn back_target(&self) -> Option<StepId> {
        let shown = self.shown();
        let cur = self.current.get();
        let i = shown.iter().position(|s| *s == cur)?;
        let prev = *shown.get(i.checked_sub(1)?)?;
        let floor_at = self.floor.get().and_then(|f| shown.iter().position(|s| *s == f));
        match floor_at {
            Some(f) if i <= f + 1 => None,
            _ => Some(prev),
        }
    }

    fn fail(&self, message: impl Into<String>) {
        self.error.set(Some(message.into()));
        self.busy.set(false);
    }
}

#[component]
pub fn SetupPage() -> impl IntoView {
    let auth = AuthState::expect();
    let toasts = Toasts::expect();
    let theme = Theme::expect();
    let navigate = use_navigate();
    let wizard = Wizard {
        answers: RwSignal::new(Answers::with_defaults()),
        ctx: RwSignal::new(Context::default()),
        current: RwSignal::new(StepId::Start),
        skipped: RwSignal::new(BTreeSet::new()),
        floor: RwSignal::new(None),
        error: RwSignal::new(None),
        busy: RwSignal::new(false),
        help: RwSignal::new(None),
        help_open: RwSignal::new(false),
    };

    // Set-up already happened. An admin can run the wizard again (Settings, then Set-up wizard),
    // without the code, the account or a restore; anyone else leaves. Runs once.
    Effect::new(move |_| {
        if auth.boot.get_untracked() == Boot::Ready {
            if auth.is_admin() {
                wizard.ctx.update(|c| c.signed_in = true);
            } else {
                let to = if auth.is_logged_in() { "/" } else { "/login" };
                navigate(to, Default::default());
            }
        }
    });

    // From another computer, the set-up code the server printed is needed once.
    spawn_local(async move {
        if let Ok(s) = api::get::<SetupStatus>("/setup").await {
            wizard.ctx.update(|c| c.code_required = s.setup_token_required);
            if let Some(el) = document().document_element() {
                let _ = el.set_attribute("data-reelix-version", &s.version);
            }
        }
    });

    let code = RwSignal::new(String::new());
    let feedback = move |error: Option<String>| {
        let step = install::step(wizard.current.get_untracked());
        if !open_feedback(step.key, step.title, error.as_deref()) {
            toasts.error("Send feedback could not open. Reload the page and try again.");
        }
    };

    let progress = move || {
        let list = wizard.shown();
        let cur = wizard.current.get();
        let pos = list.iter().position(|s| *s == cur).unwrap_or(0);
        (pos + 1, list.len())
    };

    let stepper = move || {
        let list = wizard.shown();
        let cur = wizard.current.get();
        let pos = list.iter().position(|s| *s == cur).unwrap_or(0);
        let skipped = wizard.skipped.get();
        list.into_iter()
            .enumerate()
            .map(|(i, id)| {
                let s = install::step(id);
                let state = if i == pos {
                    "current"
                } else if i < pos && skipped.contains(&StepKey::of(id)) {
                    "skipped"
                } else if i < pos {
                    "done"
                } else {
                    "todo"
                };
                let marker = match state {
                    "done" => view! { <Icon name="check" /> }.into_any(),
                    _ => (i + 1).into_any(),
                };
                let status = match state {
                    "done" => "done",
                    "skipped" => "skipped",
                    "current" => "current step",
                    _ => "to do",
                };
                view! {
                    <li class=format!("step {state}") aria-current=(state == "current").then_some("step")>
                        <span class="step-marker" aria-hidden="true">{marker}</span>
                        <span class="step-label">{s.title}<span class="visually-hidden">{format!(", {status}")}</span></span>
                    </li>
                }
            })
            .collect_view()
    };

    let progress_text = move || {
        let (n, total) = progress();
        let flow = if wizard.current.get() == StepId::Start {
            "How to start"
        } else {
            wizard.answers.with(|a| a.start()).name()
        };
        let left = match total - n {
            0 => "This is the last step.".to_owned(),
            1 => "1 step left.".to_owned(),
            l => format!("{l} steps left."),
        };
        format!("Step {n} of {total}, {flow}. {left}")
    };
    let progress_width = move || {
        let (n, total) = progress();
        format!("width: {}%", n * 100 / total.max(1))
    };

    let body = move || {
        let id = wizard.current.get();
        let s = install::step(id);
        view! {
            <div class="wizard-pane" data-component=s.key>
                <div class="wizard-progress">
                    <p class="wizard-progress-text">{progress_text}</p>
                    <div class="wizard-progress-bar" aria-hidden="true">
                        <span style=progress_width></span>
                    </div>
                </div>
                <p class="wizard-stage">{s.stage}</p>
                <h1 class="wizard-title" id="wizard-heading" tabindex="-1">{s.question}</h1>
                <p class="wizard-lead">{s.lead}</p>
                {move || wizard.error.get().map(|msg| {
                    let report = msg.clone();
                    view! {
                        <div class="alert alert-danger" role="alert">
                            <Icon name="error" />
                            <div class="alert-body">
                                <span>{msg}</span>
                                <button type="button" class="btn btn-ghost btn-sm" on:click=move |_| feedback(Some(report.clone()))>
                                    "Report this problem"
                                </button>
                            </div>
                        </div>
                    }
                })}
                <StepBody wizard=wizard id=id code=code />
                <HelpLinks wizard=wizard help=s.help />
                <StepActions wizard=wizard id=id code=code />
            </div>
        }
    };

    view! {
        <main class="wizard-page">
            <div class="wizard">
                <aside class="wizard-side">
                    <div class="auth-brand">
                        <Logo />
                        <span class="brand-name">"Reelix"</span>
                    </div>
                    <p class="wizard-side-title">"Set-up"</p>
                    <ol class="stepper">{stepper}</ol>
                    <p class="wizard-side-foot">
                        {move || auth.server_version.get().map(|v| format!("Server v{v}")).unwrap_or_default()}
                    </p>
                </aside>
                <section class="wizard-main">
                    <div class="wizard-tools">
                        <button type="button" class="btn btn-ghost btn-sm" on:click=move |_| theme.toggle()>
                            <Icon name="sun" />
                            "Light or dark"
                        </button>
                        <button type="button" class="btn btn-ghost btn-sm" on:click=move |_| feedback(None)>
                            <Icon name="heart" />
                            "Send feedback"
                        </button>
                    </div>
                    {body}
                </section>
            </div>
            {move || wizard.help.get().map(|h| view! {
                <Modal open=wizard.help_open title=h.term>
                    <div class="wizard-help">
                        {h.text.iter().map(|p| view! { <p>{*p}</p> }).collect_view()}
                        <p class="field-hint">{match HELP_CENTRE {
                            Some(_) => "More in the Reelix help centre.",
                            None => "More help: the Reelix help centre is not online yet, so there is no link here.",
                        }}</p>
                    </div>
                </Modal>
            })}
        </main>
    }
}

/// "What is…" links for the step, each opening its help in a dialog.
#[component]
fn HelpLinks(wizard: Wizard, help: &'static [Help]) -> impl IntoView {
    (!help.is_empty()).then(|| {
        view! {
            <ul class="wizard-help-links">
                {help.iter().map(|h| view! {
                    <li>
                        <button type="button" class="link-button" on:click=move |_| { wizard.help.set(Some(h)); wizard.help_open.set(true); }>
                            <Icon name="info" />
                            {h.term}
                        </button>
                    </li>
                }).collect_view()}
            </ul>
        }
    })
}

/// A question as option cards: radio buttons, or check boxes when several answers are allowed.
#[component]
fn Choices(wizard: Wizard, question: &'static Question) -> impl IntoView {
    let kind = if question.multi { "checkbox" } else { "radio" };
    view! {
        <fieldset class="wizard-choices">
            <legend>{question.legend}</legend>
            <div class="choice-cards">
                {move || question.choices.iter().filter(|c| install::offered(question, c.value, wizard.ctx.get())).map(|c| {
                    let value = c.value;
                    let checked = move || wizard.answers.with(|a| a.has(question.key, value));
                    let on_change = move |e: ev::Event| {
                        let on = event_target_checked(&e);
                        wizard.answers.update(|a| {
                            if question.multi {
                                let picked: Vec<&str> = question
                                    .choices
                                    .iter()
                                    .map(|c| c.value)
                                    .filter(|v| if *v == value { on } else { a.has(question.key, v) })
                                    .collect();
                                a.set(question.key, picked.join(","));
                            } else {
                                a.set(question.key, value);
                            }
                        });
                    };
                    view! {
                        <label class="choice-card" class:checked=checked>
                            <input type=kind name=question.key value=value prop:checked=checked on:change=on_change />
                            <span class="choice-text">
                                <span class="choice-label">
                                    {c.label}
                                    {c.recommended.then(|| view! { <span class="choice-recommended">" (recommended)"</span> })}
                                </span>
                                <span class="choice-hint">{c.hint}</span>
                            </span>
                        </label>
                    }
                }).collect_view()}
            </div>
        </fieldset>
    }
}

/// Providers the wizard suggests but does not set up, with placeholder links for now.
#[component]
fn Providers(intro: &'static str, list: &'static [Provider]) -> impl IntoView {
    view! {
        <div class="wizard-providers">
            <p>{intro}</p>
            <ul>
                {list.iter().map(|p| view! {
                    <li>
                        <strong>{p.name}</strong>
                        <span class="muted">{format!(", {}", p.note)}</span>
                        {match p.link {
                            Some(href) => view! { <a href=href target="_blank" rel="noreferrer">"Sign up (opens in a new tab)"</a> }.into_any(),
                            None => view! { <span class="badge tone-neutral">"Link to come"</span> }.into_any(),
                        }}
                    </li>
                }).collect_view()}
            </ul>
            <p class="field-hint">{PLACEHOLDER_NOTE}</p>
        </div>
    }
}

#[component]
fn StepBody(wizard: Wizard, id: StepId, code: RwSignal<String>) -> impl IntoView {
    let s = install::step(id);
    let questions = move || {
        s.questions
            .iter()
            .filter(|q| wizard.answers.with(|a| q.asked(a)))
            .map(|q| view! { <Choices wizard=wizard question=q /> })
            .collect_view()
    };
    let extra = match id {
        StepId::Code => view! {
            <div class="field">
                <label for="setup-token">"Set-up code"</label>
                <p id="setup-token-hint" class="field-hint">"Printed in the server log when Reelix starts with no accounts."</p>
                <input id="setup-token" class="input mono" type="text" autocomplete="off" autocapitalize="none"
                    spellcheck="false" aria-describedby="setup-token-hint" data-snag-private=""
                    prop:value=move || code.get() on:input=move |e| code.set(event_target_value(&e)) />
            </div>
        }
        .into_any(),
        StepId::Account => view! { <AccountFields /> }.into_any(),
        StepId::Restore => view! {
            <div class="field">
                <label for="setup-backup">"Backup file"</label>
                <p id="setup-backup-hint" class="field-hint">"Reelix makes these in System, then Backup. Their names start with reelix-backup."</p>
                <input id="setup-backup" class="input" type="file" accept=".zip,application/zip" aria-describedby="setup-backup-hint" />
            </div>
        }
        .into_any(),
        StepId::Migrate => view! { <div class="wizard-content"><MigratePage /></div> }.into_any(),
        StepId::Tmdb => view! { <TmdbFields wizard=wizard /> }.into_any(),
        StepId::Folders => {
            // The same folders the terminal wizard suggests, for what the person follows.
            let suggest = wizard.answers.with_untracked(install::library_folders);
            view! { <div class="wizard-content"><RootFoldersCard suggest=suggest /></div> }.into_any()
        }
        StepId::Torrent | StepId::Usenet => view! { <DownloadApp wizard=wizard id=id /> }.into_any(),
        StepId::Sites => view! {
            <div class="wizard-content">
                <ProviderListCard kind=ProviderKind::Indexer title="Sites to search (Indexers)"
                    description="Pick a site from the built-in list, or add a Torznab or Newznab address." />
            </div>
        }
        .into_any(),
        StepId::MediaServers => view! {
            <div class="wizard-content">
                <ProviderListCard kind=ProviderKind::MediaServer title="Media servers"
                    description="Jellyfin, Emby or Plex. For Plex, use Sign in with Plex in the form." />
            </div>
        }
        .into_any(),
        StepId::Finish => view! { <Summary wizard=wizard /> }.into_any(),
        _ => ().into_any(),
    };
    view! {
        {questions}
        {extra}
    }
}

/// The torrent or Usenet step under its question: the providers it needs, then the app's form.
#[component]
fn DownloadApp(wizard: Wizard, id: StepId) -> impl IntoView {
    let key = if id == StepId::Torrent {
        "torrent"
    } else {
        "usenet"
    };
    let needs = move || {
        install::download_needs(id, &wizard.answers.get())
            .map(|(intro, list)| view! { <Providers intro=intro list=list /> })
    };
    // Redrawn only when the app changes, not on every answer.
    let answer = Memo::new(move |_| wizard.answers.with(|a| a.get(key).to_owned()));
    let form = move || {
        let answer = answer.get();
        install::download_client_for(&answer).map(|_| {
            let name = install::step(id).questions[0]
                .choice(&answer)
                .map(|c| c.label)
                .unwrap_or_default();
            view! {
                <div class="wizard-content">
                    <ProviderListCard kind=ProviderKind::DownloadClient title="Download apps"
                        description=format!("Choose Add download client, then {name}, and give its address and sign-in. Reelix tests the connection.") />
                </div>
            }
        })
    };
    view! {
        {needs}
        {form}
    }
}

/// The admin account's fields. Read on submit through their ids, so nothing typed is kept in
/// the wizard's state (and Send feedback never sees it).
#[component]
fn AccountFields() -> impl IntoView {
    view! {
        <div class="form">
            <div class="field">
                <label for="setup-username">"Username"</label>
                <p id="setup-username-hint" class="field-hint">"Letters, numbers, dots, dashes and underscores."</p>
                <input id="setup-username" class="input" type="text" autocomplete="username" autocapitalize="none"
                    spellcheck="false" maxlength="64" aria-describedby="setup-username-hint" data-snag-private="" />
            </div>
            <div class="field">
                <label for="setup-password">"Password"</label>
                <p id="setup-password-hint" class="field-hint">"At least 8 characters. A short sentence works well."</p>
                <input id="setup-password" class="input" type="password" autocomplete="new-password" aria-describedby="setup-password-hint" />
            </div>
            <div class="field">
                <label for="setup-confirm">"Enter the password again"</label>
                <input id="setup-confirm" class="input" type="password" autocomplete="new-password" />
            </div>
        </div>
    }
}

fn input_element(id: &str) -> Option<web_sys::HtmlInputElement> {
    document()
        .get_element_by_id(id)
        .and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().ok())
}

fn input_value(id: &str) -> String {
    input_element(id).map(|i| i.value()).unwrap_or_default()
}

fn clear_input(id: &str) {
    if let Some(i) = input_element(id) {
        i.set_value("");
    }
}

#[component]
fn TmdbFields(wizard: Wizard) -> impl IntoView {
    let result = RwSignal::new(None::<Result<String, String>>);
    let test = move |_| {
        let token = input_value("setup-tmdb").trim().to_owned();
        if token.is_empty() {
            wizard.fail("Enter the read access token first.");
            return;
        }
        spawn_local(async move {
            let body = MetadataSettingsDto {
                tmdb_read_token: Some(token),
                ..MetadataSettingsDto::default()
            };
            match api::post::<_, TestResultDto>("/settings/metadata/test", &body).await {
                Ok(r) if r.ok => result.set(Some(Ok("The token works.".into()))),
                Ok(_) => result.set(Some(Err("TMDB did not accept the token. Copy it again.".into()))),
                Err(e) => result.set(Some(Err(e.message))),
            }
        });
    };
    view! {
        <ol class="wizard-list">
            <li><a href="https://www.themoviedb.org/signup" target="_blank" rel="noreferrer">"Create a free TMDB account (opens in a new tab)"</a>"."</li>
            <li>"Go to Settings, then API, and request a key for personal use."</li>
            <li>"Copy the API Read Access Token, the longer of the two."</li>
        </ol>
        <div class="field">
            <label for="setup-tmdb">"Read access token"</label>
            <p id="setup-tmdb-hint" class="field-hint">"Reelix keeps it on this server."</p>
            <input id="setup-tmdb" class="input mono" type="password" autocomplete="off" spellcheck="false" aria-describedby="setup-tmdb-hint" />
        </div>
        <div class="wizard-inline">
            <button type="button" class="btn btn-secondary" on:click=test>"Test the token"</button>
            <span role="status">
                {move || result.get().map(|r| match r {
                    Ok(m) => view! { <span class="badge tone-success">{m}</span> }.into_any(),
                    Err(m) => view! { <span class="badge tone-danger">{m}</span> }.into_any(),
                })}
            </span>
        </div>
    }
}

/// What was set and what was skipped.
#[component]
fn Summary(wizard: Wizard) -> impl IntoView {
    let rows = move || {
        let answers = wizard.answers.get();
        let skipped = wizard.skipped.get();
        wizard
            .shown()
            .into_iter()
            .filter(|id| !matches!(id, StepId::Start | StepId::Code | StepId::Finish))
            .map(|id| {
                let s = install::step(id);
                let skipped = skipped.contains(&StepKey::of(id));
                let said: Vec<&str> = s
                    .questions
                    .iter()
                    .filter(|q| q.asked(&answers))
                    .flat_map(|q| {
                        answers
                            .get(q.key)
                            .split(',')
                            .filter_map(|v| q.choice(v).map(|c| c.label))
                            .collect::<Vec<_>>()
                    })
                    .collect();
                let text = if skipped {
                    "You can do this later in Settings.".to_owned()
                } else if said.is_empty() {
                    "What you added is in Settings.".to_owned()
                } else {
                    said.join(", ")
                };
                let badge = if skipped {
                    view! { <span class="badge tone-warning">"Skipped"</span> }.into_any()
                } else if said.is_empty() {
                    view! { <span class="badge tone-neutral">"Done"</span> }.into_any()
                } else {
                    view! { <span class="badge tone-success">"Set"</span> }.into_any()
                };
                view! {
                    <tr>
                        <th scope="row">{s.title}</th>
                        <td>{badge}</td>
                        <td>{text}</td>
                    </tr>
                }
            })
            .collect_view()
    };
    let restore = wizard.answers.with_untracked(|a| a.start() == Start::Restore);
    if restore {
        view! {
            <div class="alert alert-info" role="status">
                <Icon name="info" />
                <div class="alert-body">
                    <span>"The backup is ready to restore. Restart Reelix to finish: on the server, run docker restart reelix. Then sign in with an account from the backup."</span>
                </div>
            </div>
        }
        .into_any()
    } else {
        view! {
            <div class="table-wrap">
                <table class="table">
                    <thead><tr><th scope="col">"Step"</th><th scope="col">"State"</th><th scope="col">"What you chose"</th></tr></thead>
                    <tbody>{rows}</tbody>
                </table>
            </div>
        }
        .into_any()
    }
}

#[component]
fn StepActions(wizard: Wizard, id: StepId, code: RwSignal<String>) -> impl IntoView {
    let auth = AuthState::expect();
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let s = install::step(id);

    let primary_label = match id {
        StepId::Code | StepId::Migrate => "Continue",
        StepId::Account => "Create account",
        StepId::Restore => "Restore the backup",
        StepId::Finish if wizard.answers.with_untracked(|a| a.start() == Start::Restore) => "Close",
        StepId::Finish => "Go to the dashboard",
        _ => "Save and continue",
    };

    let on_primary = move |_| {
        if wizard.busy.get_untracked() {
            return;
        }
        let answers = wizard.answers.get_untracked();
        match id {
            StepId::Account => {
                let username = input_value("setup-username").trim().to_owned();
                let password = input_value("setup-password");
                if let Some(msg) = account_problem(&username, &password, &input_value("setup-confirm")) {
                    wizard.fail(msg);
                    return;
                }
                let req = SetupRequest {
                    username,
                    password,
                    email: None,
                    setup_token: Some(code.get_untracked().trim().to_owned()).filter(|t| !t.is_empty()),
                };
                wizard.busy.set(true);
                spawn_local(async move {
                    match api::post::<_, UserDto>("/setup", &req).await {
                        Ok(user) => {
                            toasts.success(format!("Admin account {} created.", user.username));
                            auth.sign_in(user);
                            clear_input("setup-password");
                            clear_input("setup-confirm");
                            wizard.floor.set(Some(StepId::Account));
                            wizard.busy.set(false);
                            wizard.done();
                        }
                        Err(e) if e.status == 403 => {
                            wizard.go(StepId::Code);
                            wizard.fail("The set-up code is not right. Copy it again from the installer's output, or run docker logs reelix on the server.");
                        }
                        Err(e) if e.status == 409 => {
                            wizard.fail("Reelix is already set up on this server. Sign in instead.")
                        }
                        Err(e) => wizard.fail(e.message),
                    }
                });
            }
            StepId::Restore => {
                let Some(file) = input_element("setup-backup")
                    .and_then(|i| i.files())
                    .and_then(|f| f.get(0))
                else {
                    wizard.fail("Choose a backup file first.");
                    return;
                };
                let token = code.get_untracked().trim().to_owned();
                wizard.busy.set(true);
                spawn_local(async move {
                    let headers: Vec<(&str, &str)> = if token.is_empty() {
                        vec![]
                    } else {
                        vec![("x-reelix-setup-token", token.as_str())]
                    };
                    match api::post_file_with::<PendingRestoreDto>("/setup/restore", &file, &headers).await {
                        Ok(_) => {
                            wizard.floor.set(Some(StepId::Restore));
                            wizard.busy.set(false);
                            wizard.done();
                        }
                        Err(e) if e.status == 403 => {
                            wizard.go(StepId::Code);
                            wizard.fail(
                                "The set-up code is not right. Copy it again from the installer's output.",
                            );
                        }
                        Err(e) => wizard.fail(format!("The backup could not be restored: {}", e.message)),
                    }
                });
            }
            StepId::Tmdb => {
                let token = input_value("setup-tmdb").trim().to_owned();
                if token.is_empty() {
                    wizard.fail("Enter the read access token, or choose Skip this step.");
                    return;
                }
                save_then_next(wizard, async move {
                    let mut m = api::get::<MetadataSettingsDto>("/settings/metadata").await?;
                    m.tmdb_read_token = Some(token);
                    api::put::<_, MetadataSettingsDto>("/settings/metadata", &m).await?;
                    clear_input("setup-tmdb");
                    Ok(())
                });
            }
            StepId::DownloadOptions => {
                let prefer = if answers.get("prefer") == "torrent" {
                    Protocol::Torrent
                } else {
                    Protocol::Usenet
                };
                save_then_next(wizard, async move {
                    let profiles = api::get::<Vec<DelayProfile>>("/delayprofiles").await?;
                    // The profile with no tags is the default every title uses.
                    if let Some(mut p) = profiles.into_iter().find(|p| p.tags.is_empty()) {
                        p.preferred_protocol = prefer;
                        api::put::<_, DelayProfile>(&format!("/delayprofiles/{}", p.id), &p).await?;
                    }
                    Ok(())
                });
            }
            StepId::Quality => {
                let name = install::quality_profile_for(answers.get("quality"));
                save_then_next(wizard, async move {
                    let profiles = api::get::<Vec<QualityProfile>>("/qualityprofiles").await?;
                    let id = profiles.iter().find(|p| p.name == name).map(|p| p.id);
                    let mut r = api::get::<RequestSettingsDto>("/settings/requests").await?;
                    r.movie.profile_id = id;
                    r.series.profile_id = id;
                    api::put::<_, RequestSettingsDto>("/settings/requests", &r).await?;
                    Ok(())
                });
            }
            StepId::Monitor => {
                let when = install::minimum_availability_for(answers.get("movie_when"));
                save_then_next(wizard, async move {
                    let mut r = api::get::<RequestSettingsDto>("/settings/requests").await?;
                    r.minimum_availability = when.to_owned();
                    api::put::<_, RequestSettingsDto>("/settings/requests", &r).await?;
                    Ok(())
                });
            }
            StepId::Requests => {
                let auto = answers.get("requests") == "auto";
                save_then_next(wizard, async move {
                    let mut r = api::get::<RequestSettingsDto>("/settings/requests").await?;
                    r.auto_approve_movie = auto;
                    r.auto_approve_series = auto;
                    api::put::<_, RequestSettingsDto>("/settings/requests", &r).await?;
                    Ok(())
                });
            }
            StepId::Finish => {
                if answers.start() == Start::Restore {
                    toasts.info("Restart Reelix to finish restoring.");
                } else {
                    navigate("/", Default::default());
                }
            }
            _ => wizard.done(),
        }
    };

    view! {
        <div class="wizard-actions">
            {move || match wizard.back_target() {
                Some(b) => view! { <button type="button" class="btn btn-ghost" on:click=move |_| wizard.go(b)>"Back"</button> }.into_any(),
                None => view! { <span></span> }.into_any(),
            }}
            <div class="wizard-actions-end">
                {s.optional.then(|| view! {
                    <button type="button" class="btn btn-ghost" on:click=move |_| wizard.skip()>"Skip this step"</button>
                })}
                <button type="button" class="btn btn-primary btn-lg" disabled=move || wizard.busy.get() on:click=on_primary>
                    {move || if wizard.busy.get() { "Saving" } else { primary_label }}
                    {(id != StepId::Finish).then(|| view! { <Icon name="chevron" /> })}
                </button>
            </div>
        </div>
    }
}

/// Runs `save`, then moves on; an error stays on the step, beside a Report this problem button.
fn save_then_next(wizard: Wizard, save: impl std::future::Future<Output = api::ApiResult<()>> + 'static) {
    wizard.busy.set(true);
    spawn_local(async move {
        match save.await {
            Ok(()) => {
                wizard.busy.set(false);
                wizard.done();
            }
            Err(e) => wizard.fail(format!("Reelix could not save this step: {}", e.message)),
        }
    });
}

/// The account form's first problem, or `None` when it can be sent.
fn account_problem(username: &str, password: &str, confirm: &str) -> Option<&'static str> {
    if username.is_empty() || username.chars().count() > 64 {
        Some("Enter a username of 1 to 64 characters.")
    } else if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        Some("A username can only have letters, numbers, dots, dashes and underscores.")
    } else if password.chars().count() < 8 {
        Some("The password needs at least 8 characters.")
    } else if password != confirm {
        Some("The passwords are different. Enter it again.")
    } else {
        None
    }
}
