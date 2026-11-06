//! `/system/tasks` — scheduled tasks with "Run now".

use leptos::prelude::*;
use leptos::task::spawn_local;
use reelix_api_types::{BackgroundWorkDto, DomainEvent, EnqueuedJob, TaskDto};
use reelix_core::jobs::BackgroundPace;

use super::require_admin;
use crate::api;
use crate::components::{
    Card, DataTable, EmptyState, ErrorBox, Icon, PageHeader, Skeleton, StatusBadge, Tone,
};
use crate::format;
use crate::state::Toasts;
use crate::ws::LiveEvents;

#[component]
pub fn TasksPage() -> impl IntoView {
    require_admin();
    let toasts = Toasts::expect();
    let live = LiveEvents::expect();
    let tasks = LocalResource::new(|| api::get::<Vec<TaskDto>>("/system/tasks"));
    // Kinds with a "Run now" request in flight.
    let starting = RwSignal::new(Vec::<String>::new());

    Effect::new(move |prev: Option<u64>| {
        let latest = live.latest();
        let seq = latest.as_ref().map_or(0, |e| e.seq);
        if let (Some(prev), Some(ev)) = (prev, latest)
            && ev.seq != prev
            && matches!(
                ev.event,
                DomainEvent::TaskUpdated { .. }
                    | DomainEvent::JobFinished { .. }
                    | DomainEvent::JobStarted { .. }
            )
        {
            tasks.refetch();
        }
        seq
    });

    let run_now = move |kind: String, name: String| {
        starting.update(|s| s.push(kind.clone()));
        spawn_local(async move {
            match api::post_empty::<EnqueuedJob>(&format!("/system/tasks/{kind}/run")).await {
                Ok(EnqueuedJob { job_id: Some(id) }) => {
                    toasts.success(format!("{name} queued as job #{id}."))
                }
                Ok(EnqueuedJob { job_id: None }) => {
                    toasts.info(format!("{name} is already queued or running."))
                }
                Err(e) => toasts.error(format!("Could not start {name}: {e}")),
            }
            starting.update(|s| s.retain(|k| k != &kind));
            tasks.refetch();
        });
    };

    let table = move || {
        match tasks.get() {
        None => view! { <Skeleton lines=5 /> }.into_any(),
        Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| tasks.refetch()) /> }.into_any(),
        Some(Ok(list)) if list.is_empty() => {
            view! { <EmptyState icon="clock" title="No scheduled tasks" compact=true /> }.into_any()
        }
        Some(Ok(list)) => view! {
            <DataTable caption="Scheduled tasks" headers=vec!["Task", "Interval", "Last run", "Duration", "Next run", "Status", ""]>
                {list
                    .into_iter()
                    .map(|task| {
                        let kind = task.kind.clone();
                        let name = task.name.clone();
                        let busy_kind = task.kind.clone();
                        let running = task.running;
                        let busy = Signal::derive(move || running || starting.with(|s| s.contains(&busy_kind)));
                        let status = if task.running {
                            view! { <StatusBadge tone=Tone::Info label="Running" dot=true /> }.into_any()
                        } else if !task.enabled {
                            view! { <StatusBadge tone=Tone::Neutral label="Disabled" /> }.into_any()
                        } else if let Some(err) = task.last_error.clone() {
                            view! { <span title=err><StatusBadge tone=Tone::Danger label="Last run failed" dot=true /></span> }.into_any()
                        } else if task.last_run.is_some() {
                            view! { <StatusBadge tone=Tone::Success label="OK" dot=true /> }.into_any()
                        } else {
                            view! { <StatusBadge tone=Tone::Neutral label="Never run" /> }.into_any()
                        };
                        view! {
                            <tr>
                                <td>
                                    <span class="cell-strong">{task.name.clone()}</span>
                                    <span class="cell-sub mono">{task.kind.clone()}</span>
                                </td>
                                <td>
                                    {format::interval(task.paced_interval_secs.max(task.interval_secs))}
                                    {(task.paced_interval_secs > task.interval_secs).then(|| view! {
                                        <span class="cell-sub">
                                            {format!("Normally {}", format::interval(task.interval_secs).to_lowercase())}
                                        </span>
                                    })}
                                </td>
                                <td title=task.last_run.map(format::datetime).unwrap_or_default()>
                                    {task.last_run.map(format::relative).unwrap_or_else(|| "Never".into())}
                                </td>
                                <td>{task.last_duration_ms.map(format::duration_ms).unwrap_or_else(|| "—".into())}</td>
                                <td title=task.next_run.map(format::datetime).unwrap_or_default()>
                                    {task.next_run.map(format::relative).unwrap_or_else(|| "—".into())}
                                </td>
                                <td>{status}</td>
                                <td class="cell-actions">
                                    <button
                                        type="button"
                                        class="btn btn-sm btn-secondary"
                                        disabled=busy
                                        aria-label=format!("Run {} now", task.name)
                                        on:click=move |_| run_now(kind.clone(), name.clone())
                                    >
                                        <Icon name="play" />
                                        {move || if busy.get() { "Running…" } else { "Run now" }}
                                    </button>
                                </td>
                            </tr>
                        }
                    })
                    .collect_view()}
            </DataTable>
        }
        .into_any(),
    }
    };

    view! {
        <PageHeader title="Tasks" subtitle="Scheduled maintenance that Reelix runs in the background.">
            <button type="button" class="btn btn-secondary btn-sm" on:click=move |_| tasks.refetch()>
                <Icon name="refresh" />
                "Refresh"
            </button>
        </PageHeader>
        <BackgroundWorkCard on_saved=Callback::new(move |()| tasks.refetch()) />
        <Card>{table}</Card>
    }
}

/// What each pace means, for the select and its hint.
fn pace_text(p: BackgroundPace) -> (&'static str, &'static str, &'static str) {
    match p {
        BackgroundPace::Normal => ("normal", "Normal", "Every task runs at its own interval."),
        BackgroundPace::Relaxed => (
            "relaxed",
            "Relaxed (5 times slower)",
            "Downloads are checked every 2.5 minutes instead of every 30 seconds.",
        ),
        BackgroundPace::Light => (
            "light",
            "Light (20 times slower)",
            "Finished downloads can take up to 10 minutes to import, and RSS sync runs at most every 20 minutes.",
        ),
        BackgroundPace::Minimal => (
            "minimal",
            "Minimal (50 times slower)",
            "Finished downloads can take up to 25 minutes to import, and RSS sync runs at most every 50 minutes.",
        ),
    }
}

/// Background work: slows every scheduled task for small servers that also do other work.
#[component]
fn BackgroundWorkCard(on_saved: Callback<()>) -> impl IntoView {
    let toasts = Toasts::expect();
    let current = LocalResource::new(|| api::get::<BackgroundWorkDto>("/system/background-work"));
    let saving = RwSignal::new(false);
    let save = move |value: String| {
        let Some(pace) = BackgroundPace::ALL.into_iter().find(|p| pace_text(*p).0 == value) else {
            return;
        };
        saving.set(true);
        spawn_local(async move {
            match api::put::<_, BackgroundWorkDto>("/system/background-work", &BackgroundWorkDto { pace })
                .await
            {
                Ok(_) => {
                    toasts.success(format!("Background work set to {}.", pace_text(pace).1));
                    current.refetch();
                    on_saved.run(());
                }
                Err(e) => toasts.error(format!("Could not change background work: {e}")),
            }
            saving.set(false);
        });
    };
    view! {
        <Card
            title="Background work"
            subtitle="Slow down scheduled tasks on a small server that also does other work. Tasks that run once a day or less often keep their own interval."
        >
            {move || match current.get() {
                None => view! { <Skeleton lines=2 /> }.into_any(),
                Some(Err(e)) => view! { <ErrorBox error=e on_retry=Callback::new(move |()| current.refetch()) /> }.into_any(),
                Some(Ok(BackgroundWorkDto { pace })) => view! {
                    <div class="field">
                        <label for="bg-pace">"Pace"</label>
                        <select
                            id="bg-pace"
                            class="input"
                            aria-describedby="bg-pace-hint"
                            disabled=move || saving.get()
                            on:change=move |e| save(event_target_value(&e))
                        >
                            {BackgroundPace::ALL
                                .into_iter()
                                .map(|p| {
                                    let (value, label, _) = pace_text(p);
                                    view! { <option value=value selected=p == pace>{label}</option> }
                                })
                                .collect_view()}
                        </select>
                        <p class="field-hint" id="bg-pace-hint">{pace_text(pace).2}</p>
                    </div>
                }
                .into_any(),
            }}
        </Card>
    }
}
