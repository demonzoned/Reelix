//! Generic provider UI: [`ProviderForm`] renders any [`ProviderSchema`] (every `FieldKind`,
//! advanced toggle, secret masks, inline Test with per-check results) and
//! [`ProviderListCard`] lists the instances of one kind with an "Add" catalog modal.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::providers::{
    FieldKind, FieldSchema, PlexPinDto, PlexPinStatusDto, Protocol, ProviderDto, ProviderInput, ProviderKind,
    ProviderSchemaDto, TagDto, TestResultDto,
};
use serde_json::{Map, Value, json};

use crate::api;
use crate::components::library_common::on_entity_changed;
use crate::components::{ConfirmDialog, EmptyState, ErrorBox, Icon, Modal, Skeleton, StatusBadge, Tone};
use crate::state::Toasts;

/// Display string of a settings value for text inputs.
fn as_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Initial settings: schema defaults overlaid with the saved values.
fn initial_values(schema: &ProviderSchemaDto, saved: Option<&Value>) -> Map<String, Value> {
    let mut m = Map::new();
    for f in &schema.schema.fields {
        if let Some(d) = &f.default {
            m.insert(f.name.clone(), d.clone());
        }
    }
    if let Some(Value::Object(s)) = saved {
        for (k, v) in s {
            m.insert(k.clone(), v.clone());
        }
    }
    m
}

fn protocol_label(p: Protocol) -> &'static str {
    p.label()
}

/// Result list for a connection test.
#[component]
pub fn TestResultView(result: TestResultDto) -> impl IntoView {
    let tone = if result.ok { "test-ok" } else { "test-fail" };
    view! {
        <div class=format!("test-result {tone}") role="status">
            <strong>{if result.ok { "Test passed" } else { "Test failed" }}</strong>
            {result.message.clone().map(|m| view! { <span class="field-hint">" — "{m}</span> })}
            <ul class="test-checks">
                {result
                    .checks
                    .into_iter()
                    .map(|c| {
                        view! {
                            <li class:ok=c.ok class:fail=!c.ok>
                                <span aria-hidden="true">{if c.ok { "✓" } else { "✗" }}</span>
                                " "
                                <span class="test-check-name">{c.name}</span>
                                {c.message.map(|m| view! { <span class="field-hint">": "{m}</span> })}
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        </div>
    }
}

/// Element id of the schema field at `index`. It comes from the field's position, not its name:
/// browsers and password managers take an input whose id holds "username", "user" or "login" for
/// the site's own sign-in and fill it with the Reelix account, even with `autocomplete="off"`.
fn field_dom_id(index: usize) -> String {
    format!("pf-field-{index}")
}

/// One schema field bound to `values`. These fields hold another app's sign-in, so every text
/// input has `autocomplete="off"` (secrets `new-password`) and the password managers' ignore hints.
#[component]
fn SchemaField(field: FieldSchema, index: usize, values: RwSignal<Map<String, Value>>) -> impl IntoView {
    let id = field_dom_id(index);
    let name = field.name.clone();
    let label = format!("{}{}", field.label, if field.required { " *" } else { "" });
    let help = field.help.clone();
    let placeholder = field.placeholder.clone().unwrap_or_default();
    let get = {
        let name = name.clone();
        move || values.with(|m| m.get(&name).cloned())
    };
    let set = {
        let name = name.clone();
        move |v: Value| {
            values.update(|m| {
                m.insert(name.clone(), v);
            })
        }
    };
    let input = match field.kind.clone() {
        FieldKind::Bool => {
            let get = get.clone();
            return view! {
                <div class="field field-check">
                    <label class="checkbox">
                        <input type="checkbox" id=id.clone()
                            prop:checked=move || get().and_then(|v| v.as_bool()).unwrap_or(false)
                            on:change=move |e| set(Value::Bool(event_target_checked(&e))) />
                        {label.clone()}
                    </label>
                    {help.clone().map(|h| view! { <p class="field-hint">{h}</p> })}
                </div>
            }
            .into_any();
        }
        FieldKind::Number => view! {
            <input id=id.clone() class="input" type="number" step="any" placeholder=placeholder.clone()
                autocomplete="off" data-1p-ignore="true" data-lpignore="true" data-bwignore="true" data-form-type="other"
                prop:value={ let get = get.clone(); move || as_text(get().as_ref()) }
                on:input=move |e| {
                    let s = event_target_value(&e);
                    let s = s.trim();
                    let v = if s.is_empty() {
                        Value::Null
                    } else if let Ok(i) = s.parse::<i64>() {
                        json!(i)
                    } else if let Ok(f) = s.parse::<f64>() {
                        json!(f)
                    } else {
                        Value::String(s.to_owned())
                    };
                    set(v)
                } />
        }
        .into_any(),
        FieldKind::Secret => view! {
            <input id=id.clone() class="input" type="password" autocomplete="new-password"
                data-1p-ignore="true" data-lpignore="true" data-bwignore="true" data-form-type="other"
                placeholder=if placeholder.is_empty() { "Not set".to_owned() } else { placeholder.clone() }
                prop:value={ let get = get.clone(); move || as_text(get().as_ref()) }
                on:input=move |e| set(Value::String(event_target_value(&e))) />
        }
        .into_any(),
        FieldKind::Select { options } => {
            let get = get.clone();
            view! {
                <select id=id.clone() class="input" on:change=move |e| {
                    let raw = event_target_value(&e);
                    let v = options.iter().find(|o| serde_json::to_string(&o.value).ok().as_deref() == Some(raw.as_str())).map(|o| o.value.clone()).unwrap_or(Value::Null);
                    set(v)
                }>
                    <option value="null" selected={ let get = get.clone(); move || get().is_none_or(|v| v.is_null()) }>"—"</option>
                    {options.clone().into_iter().map(|o| {
                        let key = o.value.to_string();
                        let get = get.clone();
                        let val = o.value.clone();
                        view! { <option value=key selected=move || get().as_ref() == Some(&val)>{o.label}</option> }
                    }).collect_view()}
                </select>
            }
            .into_any()
        }
        FieldKind::MultiSelect { options } => {
            view! {
                <div class="multi-select" role="group" aria-label=field.label.clone()>
                    {options.into_iter().map(|o| {
                        let get = get.clone();
                        let get2 = get.clone();
                        let set = set.clone();
                        let val = o.value.clone();
                        let val2 = o.value.clone();
                        view! {
                            <label class="checkbox">
                                <input type="checkbox"
                                    prop:checked=move || get().and_then(|v| v.as_array().cloned()).is_some_and(|a| a.contains(&val))
                                    on:change={
                                        let get = get2;
                                        move |e| {
                                            let on = event_target_checked(&e);
                                            let mut arr = get().and_then(|v| v.as_array().cloned()).unwrap_or_default();
                                            arr.retain(|x| x != &val2);
                                            if on { arr.push(val2.clone()); }
                                            set(Value::Array(arr))
                                        }
                                    } />
                                {o.label}
                            </label>
                        }
                    }).collect_view()}
                </div>
            }
            .into_any()
        }
        FieldKind::List => {
            let numeric = field
                .default
                .as_ref()
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .is_some_and(Value::is_number);
            view! {
                <textarea id=id.clone() class="input" rows="3" placeholder="One entry per line"
                    autocomplete="off" data-1p-ignore="true" data-lpignore="true" data-bwignore="true" data-form-type="other"
                    prop:value={ let get = get.clone(); move || match get() {
                        Some(Value::Array(a)) => a.iter().map(|v| as_text(Some(v))).collect::<Vec<_>>().join("\n"),
                        other => as_text(other.as_ref()),
                    } }
                    on:change=move |e| {
                        let items: Vec<Value> = event_target_value(&e)
                            .split(['\n', ','])
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(|s| match (numeric, s.parse::<i64>()) {
                                (true, Ok(n)) => json!(n),
                                _ => Value::String(s.to_owned()),
                            })
                            .collect();
                        set(Value::Array(items))
                    }></textarea>
            }
            .into_any()
        }
        FieldKind::Text | FieldKind::Url | FieldKind::Path => {
            let ty = if matches!(field.kind, FieldKind::Url) { "url" } else { "text" };
            view! {
                <input id=id.clone() class="input" type=ty placeholder=placeholder.clone()
                    autocapitalize="off" spellcheck="false" autocomplete="off" data-1p-ignore="true" data-lpignore="true" data-bwignore="true" data-form-type="other"
                    prop:value={ let get = get.clone(); move || as_text(get().as_ref()) }
                    on:input=move |e| set(Value::String(event_target_value(&e))) />
            }
            .into_any()
        }
    };
    view! {
        <div class="field">
            <label for=id>{label}</label>
            {input}
            {help.map(|h| view! { <p class="field-hint">{h}</p> })}
        </div>
    }
    .into_any()
}

/// Plex sign-in: creates a plex.tv PIN, opens the auth popup and polls for the token.
#[component]
fn PlexSignIn(values: RwSignal<Map<String, Value>>) -> impl IntoView {
    let toasts = Toasts::expect();
    let busy = RwSignal::new(false);
    let start = move |_| {
        busy.set(true);
        spawn_local(async move {
            match api::post_empty::<PlexPinDto>("/mediaservers/plex/pin").await {
                Err(e) => toasts.error(format!("Could not start Plex sign-in: {e}")),
                Ok(pin) => {
                    if let Some(w) = web_sys::window() {
                        let _ = w.open_with_url_and_target_and_features(
                            &pin.auth_url,
                            "plex-auth",
                            "width=600,height=700",
                        );
                    }
                    let mut done = false;
                    for _ in 0..90 {
                        gloo_timers::future::TimeoutFuture::new(2000).await;
                        match api::get::<PlexPinStatusDto>(&format!("/mediaservers/plex/pin/{}", pin.id))
                            .await
                        {
                            Ok(st) if st.authorized => {
                                values.update(|m| {
                                    if let Some(t) = st.token.clone() {
                                        m.insert("token".into(), Value::String(t));
                                    }
                                    let url_empty = as_text(m.get("url")).trim().is_empty();
                                    if url_empty
                                        && let Some(uri) = st
                                            .servers
                                            .iter()
                                            .find(|s| s.owned)
                                            .or(st.servers.first())
                                            .and_then(|s| s.uris.first())
                                    {
                                        m.insert("url".into(), Value::String(uri.clone()));
                                    }
                                });
                                toasts.success("Signed in to Plex; token filled in.");
                                done = true;
                                break;
                            }
                            Ok(_) => {}
                            Err(e) => {
                                toasts.error(format!("Plex sign-in failed: {e}"));
                                done = true;
                                break;
                            }
                        }
                    }
                    if !done {
                        toasts.error("Plex sign-in timed out.");
                    }
                }
            }
            busy.set(false);
        });
    };
    view! {
        <div class="field">
            <button type="button" class="btn btn-secondary" on:click=start disabled=move || busy.get()>
                {move || if busy.get() { "Waiting for Plex…" } else { "Sign in with Plex" }}
            </button>
            <p class="field-hint">"Opens plex.tv in a popup and fills in the token (and URL) automatically."</p>
        </div>
    }
}

/// Create/edit form for one provider instance.
#[component]
pub fn ProviderForm(
    kind: ProviderKind,
    schema: ProviderSchemaDto,
    #[prop(default = None)] initial: Option<ProviderDto>,
    tags: Vec<TagDto>,
    on_saved: Callback<ProviderDto>,
    on_cancel: Callback<()>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let id = initial.as_ref().map(|p| p.id);
    let name = RwSignal::new(
        initial
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| schema.schema.name.clone()),
    );
    let enabled = RwSignal::new(initial.as_ref().is_none_or(|p| p.enabled));
    let priority = RwSignal::new(initial.as_ref().map_or(25, |p| p.priority).to_string());
    let selected_tags = RwSignal::new(initial.as_ref().map(|p| p.tags.clone()).unwrap_or_default());
    let values = RwSignal::new(initial_values(&schema, initial.as_ref().map(|p| &p.settings)));
    let advanced = RwSignal::new(false);
    let testing = RwSignal::new(false);
    let saving = RwSignal::new(false);
    let result = RwSignal::new(None::<TestResultDto>);
    let implementation = schema.schema.implementation.clone();
    let has_advanced = schema.schema.fields.iter().any(|f| f.advanced);
    let is_plex = kind == ProviderKind::MediaServer && implementation == "plex";

    let input = {
        let implementation = implementation.clone();
        move || ProviderInput {
            implementation: implementation.clone(),
            name: name.get_untracked().trim().to_owned(),
            enabled: enabled.get_untracked(),
            priority: priority.get_untracked().trim().parse().unwrap_or(25),
            tags: selected_tags.get_untracked(),
            settings: Value::Object(values.get_untracked()),
        }
    };

    let test = {
        let input = input.clone();
        move |_| {
            let mut body = serde_json::to_value(input()).unwrap_or_default();
            if let (Some(id), Value::Object(m)) = (id, &mut body) {
                m.insert("id".into(), json!(id));
            }
            testing.set(true);
            result.set(None);
            spawn_local(async move {
                match api::post::<_, TestResultDto>(&format!("/{}/test", kind.path()), &body).await {
                    Ok(r) => result.set(Some(r)),
                    Err(e) => toasts.error(format!("Test failed: {e}")),
                }
                testing.set(false);
            });
        }
    };

    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let body = input();
        saving.set(true);
        spawn_local(async move {
            let res = match id {
                Some(id) => api::put::<_, ProviderDto>(&format!("/{}/{id}", kind.path()), &body).await,
                None => api::post::<_, ProviderDto>(&format!("/{}", kind.path()), &body).await,
            };
            match res {
                Ok(saved) => {
                    toasts.success(format!("Saved “{}”.", saved.name));
                    on_saved.run(saved);
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
            saving.set(false);
        });
    };

    let fields = schema.schema.fields.clone();
    let field_views = move || {
        let show_adv = advanced.get();
        fields
            .iter()
            .enumerate()
            .filter(|(_, f)| show_adv || !f.advanced)
            .map(|(index, f)| view! { <SchemaField field=f.clone() index=index values=values /> })
            .collect_view()
    };

    view! {
        <form class="form provider-form" autocomplete="off" data-1p-ignore="true" data-lpignore="true" data-bwignore="true" data-form-type="other" on:submit=save>
            {schema.schema.description.clone().map(|d| view! { <p class="field-hint">{d}</p> })}
            {schema.schema.info_url.as_deref().and_then(crate::format::http_url).map(|u| view! {
                <p class="field-hint"><a href=u target="_blank" rel="noopener noreferrer">"More info ↗"</a></p>
            })}
            <div class="field-row">
                <div class="field">
                    <label for="pf-name">"Name *"</label>
                    <input id="pf-name" class="input" type="text" required maxlength="100"
                        autocomplete="off" data-1p-ignore="true" data-lpignore="true" data-bwignore="true" data-form-type="other"
                        prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e)) />
                </div>
                <div class="field">
                    <label for="pf-priority">"Priority"</label>
                    <input id="pf-priority" class="input" type="number" min="0" max="100"
                        autocomplete="off" data-1p-ignore="true" data-lpignore="true" data-bwignore="true" data-form-type="other"
                        prop:value=move || priority.get() on:input=move |e| priority.set(event_target_value(&e)) />
                    <p class="field-hint">"Lower is preferred."</p>
                </div>
            </div>
            <div class="field field-check">
                <label class="checkbox">
                    <input type="checkbox" prop:checked=move || enabled.get()
                        on:change=move |e| enabled.set(event_target_checked(&e)) />
                    "Enabled"
                </label>
            </div>
            {(!tags.is_empty()).then(|| {
                let tags = tags.clone();
                view! {
                    <div class="field">
                        <span class="field-label">"Tags"</span>
                        <div class="multi-select" role="group" aria-label="Tags">
                            {tags.into_iter().map(|t| {
                                let tid = t.id;
                                view! {
                                    <label class="checkbox">
                                        <input type="checkbox"
                                            prop:checked=move || selected_tags.with(|s| s.contains(&tid))
                                            on:change=move |e| {
                                                let on = event_target_checked(&e);
                                                selected_tags.update(|s| { s.retain(|x| *x != tid); if on { s.push(tid); } });
                                            } />
                                        {t.label}
                                    </label>
                                }
                            }).collect_view()}
                        </div>
                    </div>
                }
            })}
            {is_plex.then(|| view! { <PlexSignIn values=values /> })}
            {field_views}
            {has_advanced.then(|| view! {
                <button type="button" class="btn btn-ghost btn-sm" on:click=move |_| advanced.update(|a| *a = !*a)>
                    {move || if advanced.get() { "Hide advanced settings" } else { "Show advanced settings" }}
                </button>
            })}
            {move || result.get().map(|r| view! { <TestResultView result=r /> })}
            <div class="form-actions">
                <button type="button" class="btn btn-ghost" on:click=move |_| on_cancel.run(())>"Cancel"</button>
                <button type="button" class="btn btn-secondary" on:click=test disabled=move || testing.get()>
                    {move || if testing.get() { "Testing…" } else { "Test" }}
                </button>
                <button type="submit" class="btn btn-primary" disabled=move || saving.get()>
                    {move || if saving.get() { "Saving…" } else { "Save" }}
                </button>
            </div>
        </form>
    }
}

/// Searchable catalog of implementations ("Add indexer" etc.).
#[component]
fn Catalog(
    kind: ProviderKind,
    schemas: Vec<ProviderSchemaDto>,
    on_pick: Callback<ProviderSchemaDto>,
) -> impl IntoView {
    let query = RwSignal::new(String::new());
    let protocol = RwSignal::new(String::new());
    let privacy = RwSignal::new(String::new());
    let has_protocol = schemas.iter().any(|s| s.protocol.is_some());
    let privacies: Vec<String> = {
        let mut v: Vec<String> = schemas.iter().filter_map(|s| s.privacy.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    let schemas = StoredValue::new(schemas);
    let filtered = move || {
        let q = query.get().to_lowercase();
        let p = protocol.get();
        let pv = privacy.get();
        schemas.with_value(|all| {
            all.iter()
                .filter(|s| {
                    q.is_empty()
                        || s.schema.name.to_lowercase().contains(&q)
                        || s.schema
                            .description
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&q)
                })
                .filter(|s| p.is_empty() || s.protocol.map(protocol_label) == Some(p.as_str()))
                .filter(|s| pv.is_empty() || s.privacy.as_deref() == Some(pv.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        })
    };
    view! {
        <div class="toolbar">
            <input class="input" type="search" placeholder=format!("Search {}s…", kind.label().to_lowercase())
                aria-label="Search implementations" autofocus
                prop:value=move || query.get() on:input=move |e| query.set(event_target_value(&e)) />
            {has_protocol.then(|| view! {
                <select class="input" aria-label="Protocol" on:change=move |e| protocol.set(event_target_value(&e))>
                    <option value="">"All protocols"</option>
                    <option value="Torrent">"Torrent"</option>
                    <option value="Usenet">"Usenet"</option>
                    <option value="Soulseek">"Soulseek"</option>
                </select>
            })}
            {(!privacies.is_empty()).then(|| view! {
                <select class="input" aria-label="Type" on:change=move |e| privacy.set(event_target_value(&e))>
                    <option value="">"All types"</option>
                    {privacies.clone().into_iter().map(|p| view! { <option value=p.clone()>{p.clone()}</option> }).collect_view()}
                </select>
            })}
        </div>
        <div class="catalog-grid">
            {move || {
                let list = filtered();
                if list.is_empty() {
                    return view! { <EmptyState icon="search" title="Nothing matches" compact=true /> }.into_any();
                }
                list.into_iter().map(|s| {
                    let pick = s.clone();
                    view! {
                        <button type="button" class="catalog-item" on:click=move |_| on_pick.run(pick.clone())>
                            <span class="catalog-name">{s.schema.name.clone()}</span>
                            <span class="chip-list">
                                {s.protocol.map(|p| view! { <span class="chip">{protocol_label(p)}</span> })}
                                {s.privacy.clone().map(|p| view! { <span class="chip">{p}</span> })}
                                {s.language.clone().map(|p| view! { <span class="chip">{p}</span> })}
                                {(s.source == "definition").then(|| view! { <span class="chip">"definition"</span> })}
                            </span>
                            {s.schema.description.clone().map(|d| view! { <span class="field-hint">{d}</span> })}
                        </button>
                    }
                }).collect_view().into_any()
            }}
        </div>
    }
}

/// Cards for every instance of `kind`, with add (catalog) / edit / test / delete.
#[component]
pub fn ProviderListCard(
    kind: ProviderKind,
    #[prop(into)] title: String,
    #[prop(optional, into)] description: Option<String>,
) -> impl IntoView {
    let toasts = Toasts::expect();
    let items = LocalResource::new(move || async move {
        api::get::<Vec<ProviderDto>>(&format!("/{}", kind.path())).await
    });
    let schemas = LocalResource::new(move || async move {
        api::get::<Vec<ProviderSchemaDto>>(&format!("/{}/schema", kind.path())).await
    });
    let tags = LocalResource::new(|| api::get::<Vec<TagDto>>("/tags"));
    on_entity_changed(
        match kind {
            ProviderKind::Indexer => &["indexer"],
            ProviderKind::DownloadClient => &["download_client"],
            ProviderKind::Notification => &["notification"],
            ProviderKind::MediaServer => &["media_server"],
            ProviderKind::ImportList => &["import_list"],
        },
        false,
        move || items.refetch(),
    );
    if kind == ProviderKind::Indexer {
        // New definitions become addable as soon as a catalog sync finishes.
        on_entity_changed(&["indexer_definitions"], false, move || schemas.refetch());
    }

    let catalog_open = RwSignal::new(false);
    let form_open = RwSignal::new(false);
    let editing = RwSignal::new(None::<(ProviderSchemaDto, Option<ProviderDto>)>);
    let delete_open = RwSignal::new(false);
    let delete_target = RwSignal::new(None::<ProviderDto>);
    let testing = RwSignal::new(None::<i64>);
    let last_test = RwSignal::new(None::<(i64, TestResultDto)>);

    let open_form = move |schema: ProviderSchemaDto, initial: Option<ProviderDto>| {
        editing.set(Some((schema, initial)));
        catalog_open.set(false);
        form_open.set(true);
    };

    let edit = move |p: ProviderDto| {
        let all = schemas.get().and_then(Result::ok).unwrap_or_default();
        match all
            .into_iter()
            .find(|s| s.schema.implementation == p.implementation)
        {
            Some(s) => open_form(s, Some(p)),
            None => toasts.error(format!(
                "Implementation `{}` is no longer available.",
                p.implementation
            )),
        }
    };

    let run_test = move |id: i64| {
        testing.set(Some(id));
        spawn_local(async move {
            match api::post_empty::<TestResultDto>(&format!("/{}/{id}/test", kind.path())).await {
                Ok(r) => {
                    if r.ok {
                        toasts.success("Test passed.");
                    } else {
                        toasts.error(format!("Test failed: {}", r.message.clone().unwrap_or_default()));
                    }
                    last_test.set(Some((id, r)));
                    items.refetch();
                }
                Err(e) => toasts.error(format!("Test failed: {e}")),
            }
            testing.set(None);
        });
    };

    let confirm_delete = Callback::new(move |()| {
        let Some(p) = delete_target.get_untracked() else {
            return;
        };
        spawn_local(async move {
            match api::delete(&format!("/{}/{}", kind.path(), p.id)).await {
                Ok(()) => {
                    toasts.success(format!("Deleted “{}”.", p.name));
                    items.refetch();
                }
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    });

    let list = move || {
        match items.get() {
        None => view! { <Skeleton lines=3 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| items.refetch()) /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => view! {
            <EmptyState icon="plus" title=format!("No {}s yet", kind.label().to_lowercase())
                description="Use the Add button to configure one." compact=true />
        }
        .into_any(),
        Some(Ok(list)) => view! {
            <div class="cf-grid">
                {list.into_iter().map(|p| {
                    let pe = p.clone();
                    let pd = p.clone();
                    let pid = p.id;
                    let (tone, label) = if !p.enabled {
                        (Tone::Neutral, "Disabled".to_owned())
                    } else if p.error.is_some() {
                        (Tone::Danger, "Error".to_owned())
                    } else if p.status.as_ref().is_some_and(|s| s.disabled_until.is_some()) {
                        (Tone::Warning, "Backing off".to_owned())
                    } else {
                        (Tone::Success, "Enabled".to_owned())
                    };
                    view! {
                        <article class="cf-card provider-card">
                            <header class="cf-card-head">
                                <h3 class="cf-name">{p.name.clone()}</h3>
                                <StatusBadge tone=tone label=label dot=true />
                            </header>
                            <div class="chip-list">
                                <span class="chip">{p.implementation_name.clone()}</span>
                                {p.protocol.map(|pr| view! { <span class="chip">{protocol_label(pr)}</span> })}
                                <span class="chip">{format!("Priority {}", p.priority)}</span>
                            </div>
                            {p.error.clone().map(|e| view! { <p class="field-hint text-danger">{e}</p> })}
                            {p.status.clone().and_then(|s| s.disabled_until.map(|u| (u, s.last_failure_reason))).map(|(u, r)| view! {
                                <p class="field-hint">{format!("Disabled until {} — {}", crate::format::datetime(u), r.unwrap_or_default())}</p>
                            })}
                            {move || last_test.get().filter(|(i, _)| *i == pid).map(|(_, r)| view! { <TestResultView result=r /> })}
                            <div class="cf-actions">
                                <button type="button" class="btn btn-sm btn-secondary" on:click=move |_| edit(pe.clone())>"Edit"</button>
                                <button type="button" class="btn btn-sm btn-ghost" disabled=move || testing.get() == Some(pid)
                                    on:click=move |_| run_test(pid)>
                                    {move || if testing.get() == Some(pid) { "Testing…" } else { "Test" }}
                                </button>
                                <button type="button" class="btn btn-sm btn-ghost btn-danger-text" aria-label=format!("Delete {}", p.name)
                                    on:click=move |_| { delete_target.set(Some(pd.clone())); delete_open.set(true); }>
                                    <Icon name="trash" />
                                </button>
                            </div>
                        </article>
                    }
                }).collect_view()}
            </div>
        }
        .into_any(),
    }
    };

    let form_title = Signal::derive(move || {
        editing.with(|e| match e {
            Some((s, Some(_))) => format!("Edit {}", s.schema.name),
            Some((s, None)) => format!("Add {}", s.schema.name),
            None => String::new(),
        })
    });

    view! {
        <section class="card">
            <header class="card-header">
                <div>
                    <h2 class="card-title">{title}</h2>
                    {description.map(|d| view! { <p class="card-subtitle">{d}</p> })}
                </div>
                <div class="card-actions">
                    <button type="button" class="btn btn-primary btn-sm" on:click=move |_| catalog_open.set(true)>
                        <Icon name="plus" />
                        {format!("Add {}", kind.label().to_lowercase())}
                    </button>
                </div>
            </header>
            <div class="card-body">{list}</div>
        </section>

        <Modal open=catalog_open title=format!("Add {}", kind.label().to_lowercase())>
            {move || match schemas.get() {
                None => view! { <Skeleton lines=4 /> }.into_any(),
                Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
                Some(Ok(list)) => view! {
                    {(kind == ProviderKind::Indexer && !list.iter().any(|s| s.source == "definition"))
                        .then(|| view! { <DefinitionsEmptyHint /> })}
                    <Catalog kind=kind schemas=list on_pick=Callback::new(move |s| open_form(s, None)) />
                }
                .into_any(),
            }}
        </Modal>

        <Modal open=form_open title="Provider settings">
            {move || {
                editing.get().map(|(schema, initial)| {
                    let tags = tags.get().and_then(Result::ok).unwrap_or_default();
                    view! {
                        <h3 class="modal-subtitle">{move || form_title.get()}</h3>
                        <ProviderForm kind=kind schema=schema initial=initial tags=tags
                            on_saved=Callback::new(move |_| { form_open.set(false); items.refetch(); })
                            on_cancel=Callback::new(move |()| form_open.set(false)) />
                    }
                })
            }}
        </Modal>

        <ConfirmDialog
            open=delete_open
            title="Delete?"
            message=Signal::derive(move || delete_target.get().map(|p| format!("Delete “{}”? This cannot be undone.", p.name)).unwrap_or_default())
            confirm_label="Delete"
            on_confirm=confirm_delete
        />
    }
}

/// Shown in the Add Indexer catalog when no YAML definitions are loaded (e.g. first start
/// before the catalog finished downloading).
#[component]
fn DefinitionsEmptyHint() -> impl IntoView {
    let toasts = Toasts::expect();
    let busy = RwSignal::new(false);
    let sync = move |_| {
        busy.set(true);
        spawn_local(async move {
            if let Err(e) =
                api::post_empty::<reelix_api_types::EnqueuedJob>("/indexers/definitions/sync").await
            {
                busy.set(false);
                toasts.error(format!("Could not start the sync: {e}"));
            }
        });
    };
    view! {
        <EmptyState icon="download" title="No indexer definitions loaded" compact=true
            description="Site-specific indexers come from the community definition catalog, downloaded at runtime. Generic Torznab and Newznab indexers below work without it.">
            <button type="button" class="btn btn-primary btn-sm" on:click=sync disabled=move || busy.get()>
                {move || if busy.get() { "Syncing…" } else { "Sync definitions" }}
            </button>
            <leptos_router::components::A href="/settings/indexers" attr:class="btn btn-ghost btn-sm">"Definition settings"</leptos_router::components::A>
        </EmptyState>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words that make browsers and password managers treat an input as the site's sign-in.
    const SIGN_IN_WORDS: [&str; 5] = ["user", "login", "email", "pass", "account"];

    #[test]
    fn field_ids_do_not_name_a_sign_in() {
        for index in 0..50 {
            let id = field_dom_id(index);
            assert!(id.starts_with("pf-field-"), "{id}");
            for word in SIGN_IN_WORDS {
                assert!(!id.contains(word), "{id} holds {word}");
            }
        }
        assert_ne!(field_dom_id(0), field_dom_id(1));
    }

    /// Every text input in the form opts out of autofill, so a download client's username and
    /// password are never filled with the Reelix sign-in.
    #[test]
    fn every_text_input_opts_out_of_autofill() {
        let source = include_str!("provider_form.rs");
        let source = &source[..source.find("#[cfg(test)]").unwrap()];
        let schema_field = &source[source.find("fn SchemaField(").unwrap()..];
        let form = &source[source.find("pub fn ProviderForm(").unwrap()..];
        let form = &form[..form.find("\n}\n").unwrap()];
        let schema_field = &schema_field[..schema_field.find("\n}\n").unwrap()];
        let mut checked = 0;
        for part in [schema_field, form] {
            for tag in ["<input ", "<textarea ", "<form "] {
                for (at, _) in part.match_indices(tag) {
                    // The attributes come before the first property or event binding.
                    let rest = &part[at..];
                    let end = ["prop:", "on:", "/>"]
                        .iter()
                        .filter_map(|m| rest.find(m))
                        .min()
                        .unwrap();
                    let attrs = &rest[..end];
                    if attrs.contains("type=\"checkbox\"") {
                        continue;
                    }
                    assert!(attrs.contains("autocomplete="), "no autocomplete: {attrs}");
                    for hint in ["data-1p-ignore", "data-lpignore", "data-bwignore"] {
                        assert!(attrs.contains(hint), "no {hint}: {attrs}");
                    }
                    assert!(
                        !attrs.contains(" name="),
                        "a name attribute invites autofill: {attrs}"
                    );
                    checked += 1;
                }
            }
        }
        // Number, secret, list and text fields; the form, its name and its priority.
        assert_eq!(checked, 7);
    }
}
