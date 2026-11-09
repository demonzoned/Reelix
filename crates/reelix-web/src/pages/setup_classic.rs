//! `/setup` when Reelix is built with the `classic-setup` feature: the classic first-run wizard: create the admin account, then optionally connect a media server,
//! root folders, a download client and indexers and pick request defaults. Every step after the
//! account reuses the matching settings component and can be skipped.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use reelix_api_types::providers::ProviderKind;
use reelix_api_types::requests::RequestSettingsDto;
use reelix_api_types::{SetupRequest, SetupStatus, UserDto};

use crate::api;
use crate::components::provider_form::ProviderListCard;
use crate::components::{Card, ErrorBox, Icon, Logo, Skeleton};
use crate::pages::settings::media_management::RootFoldersCard;
use crate::pages::settings::requests::DefaultsEditor;
use crate::state::{AuthState, Boot, Toasts};

struct Step {
    title: &'static str,
    blurb: &'static str,
}

const STEPS: &[Step] = &[
    Step {
        title: "Welcome",
        blurb: "",
    },
    Step {
        title: "Admin account",
        blurb: "",
    },
    Step {
        title: "Media server",
        blurb: "Connect Jellyfin, Emby or Plex so Reelix can refresh libraries after imports and track availability for requests.",
    },
    Step {
        title: "Root folders",
        blurb: "Tell Reelix where your movies, series, books and music live on disk. Add at least one folder for each type you plan to manage.",
    },
    Step {
        title: "Download client",
        blurb: "Hook up qBittorrent, Transmission, Deluge, rTorrent, SABnzbd, NZBGet or a blackhole folder. Use Test to check the connection.",
    },
    Step {
        title: "Indexers",
        blurb: "Add Torznab/Newznab indexers or pick a site from the built-in definition catalog.",
    },
    Step {
        title: "Default profiles",
        blurb: "Choose the quality profile and root folder that approved requests and list imports start with. You can change these per title.",
    },
    Step {
        title: "Finish",
        blurb: "",
    },
];

const ACCOUNT: usize = 1;
const FINISH: usize = STEPS.len() - 1;

fn validate_username(u: &str) -> Option<&'static str> {
    if u.is_empty() || u.chars().count() > 64 {
        Some("Use 1–64 characters.")
    } else if !u
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        Some("Only letters, digits, dots, dashes and underscores are allowed.")
    } else {
        None
    }
}

#[component]
pub fn SetupPage() -> impl IntoView {
    let auth = AuthState::expect();
    let toasts = Toasts::expect();
    let navigate = use_navigate();
    let step = RwSignal::new(0usize);
    let account_done = RwSignal::new(false);

    // Setup already happened. An admin can run it again from Settings, then Set-up Wizard,
    // starting after the account; anyone else leaves. Runs once.
    {
        let navigate = navigate.clone();
        Effect::new(move |_| {
            if auth.boot.get_untracked() == Boot::Ready {
                if auth.is_admin() {
                    account_done.set(true);
                    step.set(ACCOUNT + 1);
                } else {
                    let to = if auth.is_logged_in() { "/" } else { "/login" };
                    navigate(to, Default::default());
                }
            }
        });
    }

    // Remote browsers must enter the one-time token the server printed in its log.
    let status = LocalResource::new(|| api::get::<SetupStatus>("/setup"));
    let token_required = move || matches!(status.get(), Some(Ok(s)) if s.setup_token_required);
    let setup_token = RwSignal::new(String::new());
    let username = RwSignal::new(String::new());
    let email = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let confirm = RwSignal::new(String::new());
    let touched = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

    let username_err = move || {
        touched
            .get()
            .then(|| validate_username(username.get().trim()))
            .flatten()
    };
    let password_err = move || {
        (touched.get() && password.with(|p| p.chars().count() < 8)).then_some("Use at least 8 characters.")
    };
    let confirm_err =
        move || (touched.get() && password.get() != confirm.get()).then_some("Passwords don't match.");
    let token_err = move || {
        (touched.get() && token_required() && setup_token.with(|t| t.trim().is_empty()))
            .then_some("Copy the setup token from the server log.")
    };

    let create_account = move |e: ev::SubmitEvent| {
        e.prevent_default();
        touched.set(true);
        if busy.get_untracked()
            || username_err().is_some()
            || password_err().is_some()
            || confirm_err().is_some()
            || token_err().is_some()
        {
            return;
        }
        let email_value = email.get_untracked().trim().to_string();
        let req = SetupRequest {
            username: username.get_untracked().trim().to_string(),
            password: password.get_untracked(),
            email: (!email_value.is_empty()).then_some(email_value),
            setup_token: Some(setup_token.get_untracked().trim().to_string()).filter(|t| !t.is_empty()),
        };
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            match api::post::<_, UserDto>("/setup", &req).await {
                Ok(user) => {
                    toasts.success(format!("Admin account “{}” created.", user.username));
                    auth.sign_in(user);
                    account_done.set(true);
                    password.set(String::new());
                    confirm.set(String::new());
                    step.set(ACCOUNT + 1);
                }
                Err(e) if e.status == 409 => {
                    error.set(Some(
                        "Setup has already been completed on this server. Sign in instead.".into(),
                    ));
                }
                Err(e) => error.set(Some(e.message)),
            }
            busy.set(false);
        });
    };

    let finish = {
        let navigate = navigate.clone();
        move |_| navigate("/", Default::default())
    };

    let stepper = move || {
        STEPS
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let state = move || {
                    let cur = step.get();
                    if i == cur {
                        "current"
                    } else if i < cur {
                        "done"
                    } else {
                        "todo"
                    }
                };
                view! {
                    <li class=move || format!("step {}", state()) aria-current=move || (state() == "current").then_some("step")>
                        <span class="step-marker" aria-hidden="true">
                            {move || if state() == "done" { view! { <Icon name="check" /> }.into_any() } else { (i + 1).into_any() }}
                        </span>
                        <span class="step-label">
                            {s.title}
                        </span>
                    </li>
                }
            })
            .collect_view()
    };

    let body = move || {
        let current = step.get();
        match current {
            0 => view! {
                <div class="wizard-pane">
                    <div class="wizard-hero">
                        <Logo />
                    </div>
                    <h1 class="wizard-title">"Welcome to Reelix"</h1>
                    <p class="wizard-lead">
                        "One server for your movie and series library, indexers, downloads and requests. "
                        "Create the administrator account, then connect your media server, folders, download client and indexers. Every step after the account can be skipped and changed later under Settings."
                    </p>
                    <ul class="feature-list">
                        <li><Icon name="movies" />"Movies & series management"</li>
                        <li><Icon name="indexers" />"Built-in indexer manager"</li>
                        <li><Icon name="requests" />"Requests & discovery for your users"</li>
                    </ul>
                    <div class="wizard-actions">
                        <span></span>
                        <button type="button" class="btn btn-primary btn-lg" autofocus on:click=move |_| step.set(1)>
                            "Get started"
                            <Icon name="chevron" />
                        </button>
                    </div>
                </div>
            }
            .into_any(),
            ACCOUNT => view! {
                <form class="wizard-pane form" on:submit=create_account novalidate>
                    <h1 class="wizard-title">"Create the admin account"</h1>
                    <p class="wizard-lead">"This account has full control over Reelix. You can add more users later."</p>
                    {move || error.get().map(|msg| view! {
                        <div class="alert alert-danger" role="alert">
                            <Icon name="error" />
                            <div class="alert-body"><span>{msg}</span></div>
                        </div>
                    })}
                    <Show when=token_required>
                        <div class="field" class:invalid=move || token_err().is_some()>
                            <label for="setup-token">"Setup token"</label>
                            <input id="setup-token" class="input mono" type="text" autocomplete="off"
                                autocapitalize="none" spellcheck="false" required
                                aria-invalid=move || token_err().is_some().to_string()
                                aria-describedby="setup-token-hint"
                                prop:value=move || setup_token.get()
                                on:input=move |e| setup_token.set(event_target_value(&e)) />
                            <p id="setup-token-hint" class="field-hint">{move || token_err().unwrap_or("Printed in the server log when Reelix starts without any users.")}</p>
                        </div>
                    </Show>
                    <div class="field" class:invalid=move || username_err().is_some()>
                        <label for="setup-username">"Username"</label>
                        <input id="setup-username" class="input" type="text" autocomplete="username"
                            autocapitalize="none" spellcheck="false" maxlength="64" required autofocus
                            aria-invalid=move || username_err().is_some().to_string()
                            aria-describedby="setup-username-hint"
                            prop:value=move || username.get()
                            on:input=move |e| username.set(event_target_value(&e)) />
                        <p id="setup-username-hint" class="field-hint">{move || username_err().unwrap_or("Letters, digits, dots, dashes and underscores.")}</p>
                    </div>
                    <div class="field">
                        <label for="setup-email">"Email " <span class="optional">"(optional)"</span></label>
                        <input id="setup-email" class="input" type="email" autocomplete="email"
                            prop:value=move || email.get()
                            on:input=move |e| email.set(event_target_value(&e)) />
                    </div>
                    <div class="field-row">
                        <div class="field" class:invalid=move || password_err().is_some()>
                            <label for="setup-password">"Password"</label>
                            <input id="setup-password" class="input" type="password" autocomplete="new-password" required
                                aria-invalid=move || password_err().is_some().to_string()
                            aria-describedby="setup-password-hint"
                                prop:value=move || password.get()
                                on:input=move |e| password.set(event_target_value(&e)) />
                            <p id="setup-password-hint" class="field-hint">{move || password_err().unwrap_or("At least 8 characters.")}</p>
                        </div>
                        <div class="field" class:invalid=move || confirm_err().is_some()>
                            <label for="setup-confirm">"Confirm password"</label>
                            <input id="setup-confirm" class="input" type="password" autocomplete="new-password" required
                                aria-invalid=move || confirm_err().is_some().to_string()
                            aria-describedby="setup-confirm-hint"
                                prop:value=move || confirm.get()
                                on:input=move |e| confirm.set(event_target_value(&e)) />
                            <p id="setup-confirm-hint" class="field-hint">{move || confirm_err().unwrap_or(" ")}</p>
                        </div>
                    </div>
                    <div class="wizard-actions">
                        <button type="button" class="btn btn-ghost" on:click=move |_| step.set(0)>"Back"</button>
                        <button type="submit" class="btn btn-primary btn-lg" disabled=move || busy.get()>
                            {move || if busy.get() { "Creating account…" } else { "Create account" }}
                        </button>
                    </div>
                </form>
            }
            .into_any(),
            FINISH => view! {
                <div class="wizard-pane">
                    <div class="wizard-hero success"><Icon name="check" /></div>
                    <h1 class="wizard-title">"You're all set"</h1>
                    <p class="wizard-lead">
                        "Reelix is running. Anything you skipped is under Settings, and the dashboard checklist shows what's still missing."
                    </p>
                    <div class="wizard-actions">
                        <button type="button" class="btn btn-ghost" on:click=move |_| step.set(FINISH - 1)>"Back"</button>
                        <button type="button" class="btn btn-primary btn-lg" autofocus on:click=finish.clone()>
                            "Go to dashboard"
                            <Icon name="chevron" />
                        </button>
                    </div>
                </div>
            }
            .into_any(),
            i => {
                let s = &STEPS[i];
                let back_target = if i == ACCOUNT + 1 && account_done.get_untracked() { None } else { Some(i - 1) };
                let content = match i {
                    2 => view! { <ProviderListCard kind=ProviderKind::MediaServer title="Media servers"
                        description="For Plex, use “Sign in with Plex” in the form to fetch a token." /> }.into_any(),
                    3 => view! { <RootFoldersCard /> }.into_any(),
                    4 => view! { <ProviderListCard kind=ProviderKind::DownloadClient title="Download clients" /> }.into_any(),
                    5 => view! { <ProviderListCard kind=ProviderKind::Indexer title="Indexers" /> }.into_any(),
                    _ => view! { <DefaultProfiles on_saved=Callback::new(move |()| step.set(i + 1)) /> }.into_any(),
                };
                // The profiles step saves through its own button; the others persist as you go.
                let saves_itself = i == FINISH - 1;
                view! {
                    <div class="wizard-pane">
                        <h1 class="wizard-title">{s.title}</h1>
                        <p class="wizard-lead">{s.blurb}</p>
                        <div class="wizard-content">{content}</div>
                        <div class="wizard-actions">
                            {match back_target {
                                Some(b) => view! { <button type="button" class="btn btn-ghost" on:click=move |_| step.set(b)>"Back"</button> }.into_any(),
                                None => view! { <span></span> }.into_any(),
                            }}
                            <button type="button" class=if saves_itself { "btn btn-ghost" } else { "btn btn-primary" }
                                on:click=move |_| step.set(i + 1)>
                                {if saves_itself { "Skip" } else { "Continue" }}
                                <Icon name="chevron" />
                            </button>
                        </div>
                    </div>
                }
                .into_any()
            }
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
                    <p class="wizard-side-title">"First-run setup"</p>
                    <ol class="stepper">{stepper}</ol>
                    <p class="wizard-side-foot">
                        {move || auth.server_version.get().map(|v| format!("Server v{v}")).unwrap_or_default()}
                    </p>
                </aside>
                <section class="wizard-main" aria-live="polite">{body}</section>
            </div>
        </main>
    }
}

/// Default quality profile / root folder for movie and series requests. Only those two
/// defaults are changed; every other request setting is written back as loaded.
#[component]
fn DefaultProfiles(on_saved: Callback<()>) -> impl IntoView {
    let settings = LocalResource::new(|| api::get::<RequestSettingsDto>("/settings/requests"));
    view! {
        {move || match settings.get() {
            None => view! { <Skeleton lines=4 /> }.into_any(),
            Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| settings.refetch()) /> }.into_any(),
            Some(Ok(s)) => view! { <DefaultProfilesForm initial=s on_saved=on_saved /> }.into_any(),
        }}
    }
}

#[component]
fn DefaultProfilesForm(initial: RequestSettingsDto, on_saved: Callback<()>) -> impl IntoView {
    let toasts = Toasts::expect();
    let movie = RwSignal::new(initial.movie.clone());
    let series = RwSignal::new(initial.series.clone());
    let saving = RwSignal::new(false);
    let initial = StoredValue::new(initial);

    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        if saving.get_untracked() {
            return;
        }
        let mut body = initial.get_value();
        body.movie = movie.get_untracked();
        body.series = series.get_untracked();
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, RequestSettingsDto>("/settings/requests", &body).await {
                Ok(_) => {
                    toasts.success("Defaults saved.");
                    on_saved.run(());
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    view! {
        <Card title="Request defaults" subtitle="“Library default” uses the first quality profile.">
            <form id="setup-defaults" class="form" on:submit=save>
                <DefaultsEditor label="Movies" kind="movie" is_4k=false value=movie />
                <DefaultsEditor label="Series" kind="series" is_4k=false value=series />
                <div class="form-actions">
                    <button type="submit" class="btn btn-primary" disabled=move || saving.get()>
                        {move || if saving.get() { "Saving…" } else { "Save and continue" }}
                    </button>
                </div>
            </form>
        </Card>
    }
}
