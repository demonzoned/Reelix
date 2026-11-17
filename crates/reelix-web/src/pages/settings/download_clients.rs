//! `/settings/downloadclients` — download clients and remote path mappings.

use leptos::ev;
use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::providers::{ProviderKind, RemotePathMappingDto, RemotePathMappingInput};

use crate::api;
use crate::components::library_common::on_entity_changed;
use crate::components::provider_form::ProviderListCard;
use crate::components::{Card, DataTable, EmptyState, ErrorBox, Icon, Modal, PageHeader, Skeleton};
use crate::pages::system::require_admin;
use crate::state::Toasts;

#[component]
pub fn DownloadClientsPage() -> impl IntoView {
    require_admin();
    view! {
        <PageHeader title="Download Clients" subtitle="aria2, qBittorrent, Transmission, Deluge, rTorrent, SABnzbd, NZBGet and blackhole folders." />
        <div class="stack">
            <ProviderListCard kind=ProviderKind::DownloadClient title="Clients" />
            <RemotePathMappings />
        </div>
    }
}

#[component]
fn RemotePathMappings() -> impl IntoView {
    let toasts = Toasts::expect();
    let maps = LocalResource::new(|| api::get::<Vec<RemotePathMappingDto>>("/remotepathmappings"));
    on_entity_changed(&["remote_path_mapping"], false, move || maps.refetch());
    let open = RwSignal::new(false);
    let editing = RwSignal::new(None::<i64>);
    let host = RwSignal::new(String::new());
    let remote = RwSignal::new(String::new());
    let local = RwSignal::new(String::new());

    let start = move |m: Option<RemotePathMappingDto>| {
        editing.set(m.as_ref().map(|m| m.id));
        host.set(m.as_ref().map(|m| m.host.clone()).unwrap_or_default());
        remote.set(m.as_ref().map(|m| m.remote_path.clone()).unwrap_or_default());
        local.set(m.map(|m| m.local_path).unwrap_or_default());
        open.set(true);
    };
    let save = move |e: ev::SubmitEvent| {
        e.prevent_default();
        let body = RemotePathMappingInput {
            host: host.get_untracked(),
            remote_path: remote.get_untracked(),
            local_path: local.get_untracked(),
        };
        spawn_local(async move {
            let res = match editing.get_untracked() {
                Some(id) => {
                    api::put::<_, RemotePathMappingDto>(&format!("/remotepathmappings/{id}"), &body).await
                }
                None => api::post::<_, RemotePathMappingDto>("/remotepathmappings", &body).await,
            };
            match res {
                Ok(_) => {
                    toasts.success("Saved mapping.");
                    open.set(false);
                    maps.refetch();
                }
                Err(e) => toasts.error(format!("Could not save: {e}")),
            }
        });
    };
    let remove = move |id: i64| {
        spawn_local(async move {
            match api::delete(&format!("/remotepathmappings/{id}")).await {
                Ok(()) => maps.refetch(),
                Err(e) => toasts.error(format!("Could not delete: {e}")),
            }
        });
    };

    view! {
        <Card title="Remote path mappings"
            subtitle="When a client runs on another machine or container, map the paths it reports to paths Reelix can see."
            actions=view! {
                <button type="button" class="btn btn-primary btn-sm" on:click=move |_| start(None)><Icon name="plus" />"Add mapping"</button>
            }.into_any()>
            {move || match maps.get() {
                None => view! { <Skeleton lines=2 /> }.into_any(),
                Some(Err(e)) => view! { <ErrorBox error=e /> }.into_any(),
                Some(Ok(list)) if list.is_empty() => view! { <EmptyState icon="folder" title="No mappings" compact=true /> }.into_any(),
                Some(Ok(list)) => view! {
                    <DataTable caption="Remote path mappings" headers=vec!["Host", "Remote path", "Local path", ""]>
                        {list.into_iter().map(|m| {
                            let me = m.clone();
                            let id = m.id;
                            view! {
                                <tr>
                                    <td>{m.host.clone()}</td>
                                    <td><code>{m.remote_path.clone()}</code></td>
                                    <td><code>{m.local_path.clone()}</code></td>
                                    <td class="row-actions">
                                        <button type="button" class="btn btn-sm btn-ghost" on:click=move |_| start(Some(me.clone()))>"Edit"</button>
                                        <button type="button" class="btn btn-sm btn-ghost btn-danger-text" aria-label="Delete mapping"
                                            on:click=move |_| remove(id)><Icon name="trash" /></button>
                                    </td>
                                </tr>
                            }
                        }).collect_view()}
                    </DataTable>
                }.into_any(),
            }}
        </Card>
        <Modal open=open title="Remote path mapping">
            <form class="form" on:submit=save>
                <div class="field">
                    <label for="rpm-host">"Host"</label>
                    <input id="rpm-host" class="input" required placeholder="As set in the download client settings"
                        prop:value=move || host.get() on:input=move |e| host.set(event_target_value(&e)) />
                </div>
                <div class="field">
                    <label for="rpm-remote">"Remote path"</label>
                    <input id="rpm-remote" class="input" required placeholder="/downloads"
                        prop:value=move || remote.get() on:input=move |e| remote.set(event_target_value(&e)) />
                </div>
                <div class="field">
                    <label for="rpm-local">"Local path"</label>
                    <input id="rpm-local" class="input" required placeholder="/mnt/downloads"
                        prop:value=move || local.get() on:input=move |e| local.set(event_target_value(&e)) />
                </div>
                <div class="form-actions">
                    <button type="button" class="btn btn-ghost" on:click=move |_| open.set(false)>"Cancel"</button>
                    <button type="submit" class="btn btn-primary">"Save"</button>
                </div>
            </form>
        </Modal>
    }
}
