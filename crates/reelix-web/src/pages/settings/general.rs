//! `/settings/general` — API keys and the server settings stored in config.toml.

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::SystemStatus;
use reelix_api_types::system::{ServerSettingsDto, ServerSettingsValues};

use crate::api;
use crate::components::{ApiKeysPanel, Card, ErrorBox, Icon, PageHeader, Skeleton};
use crate::pages::system::require_admin;
use crate::state::Toasts;

#[component]
pub fn GeneralPage() -> impl IntoView {
    require_admin();
    let status = LocalResource::new(|| api::get::<SystemStatus>("/system/status"));
    let settings = LocalResource::new(|| api::get::<ServerSettingsDto>("/system/settings"));

    let paths = move || {
        match status.get() {
        None => view! { <Skeleton lines=2 /> }.into_any(),
        Some(Err(e)) => view! { <p class="muted">{e.message}</p> }.into_any(),
        Some(Ok(s)) => view! {
            <dl class="kv">
                <div class="kv-row"><dt>"Data directory"</dt><dd class="mono">{s.data_dir.clone().unwrap_or_default()}</dd></div>
                <div class="kv-row"><dt>"Database"</dt><dd>{s.database}</dd></div>
            </dl>
        }
        .into_any(),
    }
    };

    view! {
        <PageHeader title="General" subtitle="Security and server settings." />
        <div class="stack">
            <ApiKeysPanel />
            {move || match settings.get() {
                None => view! { <Card title="Server"><Skeleton lines=5 /></Card> }.into_any(),
                Some(Err(e)) => view! {
                    <Card title="Server">
                        <ErrorBox error=e on_retry=Callback::new(move |()| settings.refetch()) />
                    </Card>
                }
                .into_any(),
                Some(Ok(s)) => view! { <ServerForm s=s /> }.into_any(),
            }}
            <Card title="Storage" subtitle="Set with --data-dir or REELIX_DATA_DIR; the database URL is set in config.toml.">
                {paths}
            </Card>
        </div>
    }
}

#[component]
fn ServerForm(s: ServerSettingsDto) -> impl IntoView {
    let toasts = Toasts::expect();
    let bind = RwSignal::new(s.saved.bind.clone());
    let port = RwSignal::new(s.saved.port.to_string());
    let url_base = RwSignal::new(s.saved.url_base.clone());
    let log_level = RwSignal::new(s.saved.log_level.clone());
    let log_format = RwSignal::new(s.saved.log_format.clone());
    let restart = RwSignal::new(s.restart_required);
    let running = StoredValue::new(s.running.clone());
    let saving = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let level_is_custom = !["trace", "debug", "info", "warn", "error"].contains(&s.saved.log_level.as_str());

    let save = move |_| {
        error.set(None);
        let Ok(p) = port.get_untracked().trim().parse::<u16>() else {
            error.set(Some("The port must be a number between 1 and 65535.".into()));
            return;
        };
        let body = ServerSettingsValues {
            bind: bind.get_untracked(),
            port: p,
            url_base: url_base.get_untracked(),
            log_level: log_level.get_untracked(),
            log_format: log_format.get_untracked(),
        };
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, ServerSettingsDto>("/system/settings", &body).await {
                Ok(saved) => {
                    url_base.set(saved.saved.url_base.clone());
                    bind.set(saved.saved.bind.clone());
                    restart.set(saved.restart_required);
                    if saved.restart_required {
                        toasts.success("Saved to config.toml. Restart Reelix to apply the changes.");
                    } else {
                        toasts.success("Server settings saved.");
                    }
                }
                Err(e) => error.set(Some(e.message)),
            }
            saving.set(false);
        });
    };

    let running_line = move || {
        let r = running.get_value();
        let base = if r.url_base.is_empty() {
            "/".to_string()
        } else {
            r.url_base
        };
        format!(
            "Running now: {}:{} · URL base {base} · log level {} ({})",
            r.bind, r.port, r.log_level, r.log_format
        )
    };

    view! {
        <Card title="Server" subtitle=format!("Saved to {}. Changes apply after Reelix restarts.", s.config_file)>
            <div class="form">
                {(!s.overridden.is_empty()).then(|| view! {
                    <div class="alert alert-info" role="note">
                        <Icon name="info" />
                        <div class="alert-body">
                            <strong>"Some settings are set outside config.toml"</strong>
                            <span>{format!(
                                "These override what you save here: {}.",
                                s.overridden.join(", ")
                            )}</span>
                        </div>
                    </div>
                })}
                <Show when=move || restart.get()>
                    <div class="alert alert-warning" role="status">
                        <Icon name="warning" />
                        <div class="alert-body">
                            <strong>"Restart required"</strong>
                            <span>"The saved settings differ from the ones Reelix is running with. Restart Reelix to apply them."</span>
                        </div>
                    </div>
                </Show>
                <div class="field-row">
                    <div class="field">
                        <label for="srv-bind">"Bind address"</label>
                        <input id="srv-bind" class="input mono" prop:value=move || bind.get()
                            on:input=move |e| bind.set(event_target_value(&e)) />
                        <span class="field-hint">"0.0.0.0 listens on every interface (IPv4 and IPv6); 127.0.0.1 only on this computer."</span>
                    </div>
                    <div class="field">
                        <label for="srv-port">"Port"</label>
                        <input id="srv-port" class="input" type="number" min="1" max="65535" inputmode="numeric"
                            prop:value=move || port.get() on:input=move |e| port.set(event_target_value(&e)) />
                    </div>
                </div>
                <div class="field">
                    <label for="srv-base">"URL base " <span class="optional">"(optional)"</span></label>
                    <input id="srv-base" class="input mono" placeholder="/reelix" prop:value=move || url_base.get()
                        on:input=move |e| url_base.set(event_target_value(&e)) />
                    <span class="field-hint">"Serve Reelix under a sub-path behind a reverse proxy. Leave empty to serve it at the root."</span>
                </div>
                <div class="field-row">
                    <div class="field">
                        <label for="srv-level">"Log level"</label>
                        {if level_is_custom {
                            view! {
                                <input id="srv-level" class="input mono" prop:value=move || log_level.get()
                                    on:input=move |e| log_level.set(event_target_value(&e)) />
                            }
                            .into_any()
                        } else {
                            view! {
                                <select id="srv-level" class="input" prop:value=move || log_level.get()
                                    on:change=move |e| log_level.set(event_target_value(&e))>
                                    <option value="error">"Error"</option>
                                    <option value="warn">"Warning"</option>
                                    <option value="info">"Info"</option>
                                    <option value="debug">"Debug"</option>
                                    <option value="trace">"Trace"</option>
                                </select>
                            }
                            .into_any()
                        }}
                        <span class="field-hint">"Debug and trace are useful when reporting a problem, but write much more."</span>
                    </div>
                    <div class="field">
                        <label for="srv-format">"Log format"</label>
                        <select id="srv-format" class="input" prop:value=move || log_format.get()
                            on:change=move |e| log_format.set(event_target_value(&e))>
                            <option value="pretty">"Text"</option>
                            <option value="json">"JSON"</option>
                        </select>
                    </div>
                </div>
                {move || error.get().map(|e| view! { <p class="field-error" role="alert">{e}</p> })}
                <p class="field-hint">{running_line}</p>
                <div class="form-actions">
                    <button type="button" class="btn btn-primary" on:click=save disabled=move || saving.get()>
                        {move || if saving.get() { "Saving…" } else { "Save changes" }}
                    </button>
                </div>
            </div>
        </Card>
    }
}
