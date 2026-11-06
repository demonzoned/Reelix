use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use reelix_core::DomainEvent;
use reelix_core::bus::EventBus;
use reelix_core::jobs::JobStatus;
use reelix_db::{Db, DbError, NewJob, now_ms};
use reelix_jobs::{JobContext, JobHandler, JobSystem, JobSystemBuilder, TaskDef};
use serde_json::{Value, json};
use tokio::sync::broadcast::Receiver;
use tokio_util::sync::CancellationToken;

struct Counter {
    kind: &'static str,
    runs: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl JobHandler for Counter {
    fn kind(&self) -> &'static str {
        self.kind
    }
    async fn run(&self, _ctx: &JobContext, _payload: Value) -> anyhow::Result<()> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct AlwaysFails;

#[async_trait::async_trait]
impl JobHandler for AlwaysFails {
    fn kind(&self) -> &'static str {
        "fails"
    }
    async fn run(&self, _ctx: &JobContext, _payload: Value) -> anyhow::Result<()> {
        anyhow::bail!("nope")
    }
}

struct Panics;

#[async_trait::async_trait]
impl JobHandler for Panics {
    fn kind(&self) -> &'static str {
        "panics"
    }
    async fn run(&self, _ctx: &JobContext, _payload: Value) -> anyhow::Result<()> {
        panic!("kaboom")
    }
}

/// Sleeps, tracking the peak number of concurrent runs.
struct Slow {
    kind: &'static str,
    limit: usize,
    current: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl JobHandler for Slow {
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn concurrency(&self) -> usize {
        self.limit
    }
    async fn run(&self, _ctx: &JobContext, _payload: Value) -> anyhow::Result<()> {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(40)).await;
        self.current.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Counts its starts, then runs until shutdown is requested.
struct Blocks {
    kind: &'static str,
    started: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl JobHandler for Blocks {
    fn kind(&self) -> &'static str {
        self.kind
    }
    async fn run(&self, ctx: &JobContext, _payload: Value) -> anyhow::Result<()> {
        self.started.fetch_add(1, Ordering::SeqCst);
        ctx.cancel.cancelled().await;
        Ok(())
    }
}

/// Runs until shutdown is requested.
struct WaitsForCancel;

#[async_trait::async_trait]
impl JobHandler for WaitsForCancel {
    fn kind(&self) -> &'static str {
        "waits"
    }
    async fn run(&self, ctx: &JobContext, _payload: Value) -> anyhow::Result<()> {
        ctx.cancel.cancelled().await;
        Ok(())
    }
}

fn fast(db: &Db, bus: &EventBus) -> JobSystemBuilder {
    JobSystem::builder(db.clone(), bus.clone())
        .tick(Duration::from_millis(50))
        .poll_interval(Duration::from_millis(20))
        .retry_base(Duration::from_millis(10))
}

/// Waits for an event matching `f`, returning it.
async fn wait_for(rx: &mut Receiver<DomainEvent>, f: impl Fn(&DomainEvent) -> bool) -> DomainEvent {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let ev = rx.recv().await.expect("event bus closed");
            if f(&ev) {
                return ev;
            }
        }
    })
    .await
    .expect("timed out waiting for event")
}

fn finished(id: i64, status: JobStatus) -> impl Fn(&DomainEvent) -> bool {
    move |ev| matches!(ev, DomainEvent::JobFinished { id: i, status: s, .. } if *i == id && *s == status)
}

#[tokio::test]
async fn runs_handler_and_publishes_events() {
    let db = Db::in_memory().await.unwrap();
    let bus = EventBus::default();
    let mut rx = bus.subscribe();
    let runs = Arc::new(AtomicUsize::new(0));
    let sys = fast(&db, &bus)
        .handler(Counter {
            kind: "count",
            runs: runs.clone(),
        })
        .build();
    let queue = sys.queue();
    let cancel = CancellationToken::new();
    let handle = sys.start(cancel.clone());

    let id = queue
        .enqueue(NewJob::new("count", json!({"a": 1})))
        .await
        .unwrap()
        .unwrap();
    wait_for(
        &mut rx,
        |e| matches!(e, DomainEvent::JobQueued { id: i, .. } if *i == id),
    )
    .await;
    wait_for(
        &mut rx,
        |e| matches!(e, DomainEvent::JobStarted { id: i, kind } if *i == id && kind == "count"),
    )
    .await;
    let ev = wait_for(&mut rx, finished(id, JobStatus::Completed)).await;
    assert!(matches!(ev, DomainEvent::JobFinished { error: None, .. }));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let job = db.jobs().get(id).await.unwrap().unwrap();
    assert_eq!(job.status, JobStatus::Completed);
    assert!(job.finished_at.is_some());

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn failing_handler_retries_then_fails() {
    let db = Db::in_memory().await.unwrap();
    let bus = EventBus::default();
    let mut rx = bus.subscribe();
    let sys = fast(&db, &bus).handler(AlwaysFails).build();
    let queue = sys.queue();
    let cancel = CancellationToken::new();
    let handle = sys.start(cancel.clone());

    let id = queue
        .enqueue(NewJob::new("fails", json!({})))
        .await
        .unwrap()
        .unwrap();
    // Two retryable failures (reported with status Queued), then a final Failed.
    for _ in 0..2 {
        let ev = wait_for(&mut rx, finished(id, JobStatus::Queued)).await;
        assert!(matches!(ev, DomainEvent::JobFinished { error: Some(ref e), .. } if e == "nope"));
    }
    wait_for(&mut rx, finished(id, JobStatus::Failed)).await;
    let job = db.jobs().get(id).await.unwrap().unwrap();
    assert_eq!(job.status, JobStatus::Failed);
    assert_eq!(job.attempts, 3);
    assert_eq!(job.error.as_deref(), Some("nope"));

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn panicking_handler_does_not_kill_runner() {
    let db = Db::in_memory().await.unwrap();
    let bus = EventBus::default();
    let mut rx = bus.subscribe();
    let runs = Arc::new(AtomicUsize::new(0));
    let sys = fast(&db, &bus)
        .handler(Panics)
        .handler(Counter {
            kind: "count",
            runs: runs.clone(),
        })
        .build();
    let queue = sys.queue();
    let cancel = CancellationToken::new();
    let handle = sys.start(cancel.clone());

    let mut job = NewJob::new("panics", json!({}));
    job.max_attempts = 1;
    let bad = queue.enqueue(job).await.unwrap().unwrap();
    let ev = wait_for(&mut rx, finished(bad, JobStatus::Failed)).await;
    assert!(matches!(ev, DomainEvent::JobFinished { error: Some(ref e), .. } if e.contains("kaboom")));

    let good = queue
        .enqueue(NewJob::new("count", json!({})))
        .await
        .unwrap()
        .unwrap();
    wait_for(&mut rx, finished(good, JobStatus::Completed)).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn scheduler_enqueues_due_task_and_records_run() {
    let db = Db::in_memory().await.unwrap();
    let bus = EventBus::default();
    let mut rx = bus.subscribe();
    let runs = Arc::new(AtomicUsize::new(0));
    let sys = fast(&db, &bus)
        .handler(Counter {
            kind: "tick",
            runs: runs.clone(),
        })
        .task(TaskDef {
            kind: "tick",
            name: "Tick",
            interval: Duration::from_secs(3600),
            run_on_startup: true,
        })
        .task(TaskDef {
            kind: "later",
            name: "Later",
            interval: Duration::from_secs(3600),
            run_on_startup: false,
        })
        .build();
    let cancel = CancellationToken::new();
    let before = now_ms();
    let handle = sys.start(cancel.clone());

    let ev = wait_for(
        &mut rx,
        |e| matches!(e, DomainEvent::JobFinished { kind, .. } if kind == "tick"),
    )
    .await;
    assert!(matches!(
        ev,
        DomainEvent::JobFinished {
            status: JobStatus::Completed,
            ..
        }
    ));
    wait_for(
        &mut rx,
        |e| matches!(e, DomainEvent::TaskUpdated { kind } if kind == "tick"),
    )
    .await;

    let task = db.tasks().get("tick").await.unwrap().unwrap();
    assert_eq!(task.name, "Tick");
    assert!(task.last_run.is_some());
    assert!(task.last_duration_ms.is_some());
    assert!(task.last_error.is_none());
    assert!(task.next_run.unwrap().timestamp_millis() >= before + 3_600_000);

    let later = db.tasks().get("later").await.unwrap().unwrap();
    assert!(later.last_run.is_none());
    assert!(later.next_run.unwrap().timestamp_millis() >= before + 3_600_000);

    // Not re-run on following ticks.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn run_task_now_dedupes_and_rejects_unknown() {
    let db = Db::in_memory().await.unwrap();
    let bus = EventBus::default();
    let runs = Arc::new(AtomicUsize::new(0));
    let sys = fast(&db, &bus)
        .handler(Counter { kind: "tick", runs })
        .task(TaskDef {
            kind: "tick",
            name: "Tick",
            interval: Duration::from_secs(60),
            run_on_startup: false,
        })
        .build();
    let queue = sys.queue();
    assert!(matches!(
        queue.run_task_now("nope").await,
        Err(DbError::Conflict(_))
    ));
    let id = queue.run_task_now("tick").await.unwrap();
    assert!(id.is_some());
    assert_eq!(queue.run_task_now("tick").await.unwrap(), None);
    let job = db.jobs().get(id.unwrap()).await.unwrap().unwrap();
    assert_eq!(job.dedupe_key.as_deref(), Some("task:tick"));
}

#[tokio::test]
async fn crashed_running_jobs_are_requeued_on_start() {
    let db = Db::in_memory().await.unwrap();
    let bus = EventBus::default();
    let mut rx = bus.subscribe();
    let id = db
        .jobs()
        .enqueue(NewJob::new("count", json!({})))
        .await
        .unwrap()
        .unwrap();
    db.jobs().claim("count", now_ms()).await.unwrap().unwrap(); // left "running" by a crash

    let runs = Arc::new(AtomicUsize::new(0));
    let sys = fast(&db, &bus)
        .handler(Counter {
            kind: "count",
            runs: runs.clone(),
        })
        .build();
    let cancel = CancellationToken::new();
    let handle = sys.start(cancel.clone());
    wait_for(&mut rx, finished(id, JobStatus::Completed)).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn unknown_kinds_are_never_claimed() {
    let db = Db::in_memory().await.unwrap();
    let bus = EventBus::default();
    let runs = Arc::new(AtomicUsize::new(0));
    let sys = fast(&db, &bus).handler(Counter { kind: "count", runs }).build();
    let queue = sys.queue();
    let cancel = CancellationToken::new();
    let handle = sys.start(cancel.clone());
    let id = queue
        .enqueue(NewJob::new("mystery", json!({})))
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        db.jobs().get(id).await.unwrap().unwrap().status,
        JobStatus::Queued
    );
    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn concurrency_limits_are_respected() {
    let db = Db::in_memory().await.unwrap();
    let bus = EventBus::default();
    let mut rx = bus.subscribe();
    let (cur_a, peak_a) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let all = Arc::new(AtomicUsize::new(0));
    let peak_all = Arc::new(AtomicUsize::new(0));
    let sys = fast(&db, &bus)
        .global_concurrency(3)
        .handler(Slow {
            kind: "one",
            limit: 1,
            current: cur_a.clone(),
            peak: peak_a.clone(),
        })
        .handler(Slow {
            kind: "many",
            limit: 10,
            current: all.clone(),
            peak: peak_all.clone(),
        })
        .build();
    let queue = sys.queue();
    let mut ids = Vec::new();
    for _ in 0..4 {
        ids.push(
            queue
                .enqueue(NewJob::new("one", json!({})))
                .await
                .unwrap()
                .unwrap(),
        );
        ids.push(
            queue
                .enqueue(NewJob::new("many", json!({})))
                .await
                .unwrap()
                .unwrap(),
        );
    }
    let cancel = CancellationToken::new();
    let handle = sys.start(cancel.clone());
    let mut done = 0;
    while done < ids.len() {
        wait_for(&mut rx, |e| {
            matches!(
                e,
                DomainEvent::JobFinished {
                    status: JobStatus::Completed,
                    ..
                }
            )
        })
        .await;
        done += 1;
    }
    assert_eq!(peak_a.load(Ordering::SeqCst), 1);
    // "many" gets at most global(3) - 0..1 slots.
    assert!(peak_all.load(Ordering::SeqCst) <= 3);
    assert!(peak_all.load(Ordering::SeqCst) >= 2);
    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn long_running_jobs_leave_slots_for_short_jobs() {
    let db = Db::in_memory().await.unwrap();
    let bus = EventBus::default();
    let mut rx = bus.subscribe();
    let started = Arc::new(AtomicUsize::new(0));
    let runs = Arc::new(AtomicUsize::new(0));
    let mut builder = fast(&db, &bus).global_concurrency(4);
    for kind in ["refresh-a", "refresh-b", "refresh-c", "refresh-d"] {
        builder = builder.long_running_handler(Blocks {
            kind,
            started: started.clone(),
        });
    }
    let sys = builder
        .handler(Counter {
            kind: "count",
            runs: runs.clone(),
        })
        .build();
    let queue = sys.queue();
    for kind in ["refresh-a", "refresh-b", "refresh-c", "refresh-d"] {
        queue
            .enqueue(NewJob::new(kind, json!({})))
            .await
            .unwrap()
            .unwrap();
    }
    let cancel = CancellationToken::new();
    let handle = sys.start(cancel.clone());
    // Four jobs that never finish would take every slot without the reservation.
    tokio::time::timeout(Duration::from_secs(5), async {
        while started.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("long-running jobs didn't start");
    for _ in 0..3 {
        let id = queue
            .enqueue(NewJob::new("count", json!({})))
            .await
            .unwrap()
            .unwrap();
        wait_for(&mut rx, finished(id, JobStatus::Completed)).await;
    }
    assert_eq!(runs.load(Ordering::SeqCst), 3);
    assert_eq!(
        started.load(Ordering::SeqCst),
        2,
        "only global - 2 long-running jobs at once"
    );
    cancel.cancel();
    handle.await.unwrap();
}

#[tokio::test]
async fn shutdown_cancels_and_waits_for_running_jobs() {
    let db = Db::in_memory().await.unwrap();
    let bus = EventBus::default();
    let mut rx = bus.subscribe();
    let sys = fast(&db, &bus).handler(WaitsForCancel).build();
    let queue = sys.queue();
    let cancel = CancellationToken::new();
    let handle = sys.start(cancel.clone());
    let id = queue
        .enqueue(NewJob::new("waits", json!({})))
        .await
        .unwrap()
        .unwrap();
    wait_for(
        &mut rx,
        |e| matches!(e, DomainEvent::JobStarted { id: i, .. } if *i == id),
    )
    .await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        db.jobs().get(id).await.unwrap().unwrap().status,
        JobStatus::Completed
    );
}

#[tokio::test]
async fn background_pace_stretches_the_next_run_and_can_be_changed() {
    use reelix_core::jobs::{BACKGROUND_PACE_KEY, BackgroundPace};

    let db = Db::in_memory().await.unwrap();
    let bus = EventBus::default();
    let mut rx = bus.subscribe();
    // Saved before start, as after a restart.
    db.settings()
        .set(BACKGROUND_PACE_KEY, &BackgroundPace::Light)
        .await
        .unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let sys = fast(&db, &bus)
        .handler(Counter { kind: "tick", runs })
        .task(TaskDef {
            kind: "tick",
            name: "Tick",
            interval: Duration::from_secs(60),
            run_on_startup: true,
        })
        .build();
    let queue = sys.queue();
    let cancel = CancellationToken::new();
    let before = now_ms();
    let handle = sys.start(cancel.clone());
    wait_for(
        &mut rx,
        |e| matches!(e, DomainEvent::JobFinished { kind, .. } if kind == "tick"),
    )
    .await;
    assert_eq!(queue.pace(), BackgroundPace::Light);
    // Light: 20 times the 60 second interval.
    let next = db.tasks().get("tick").await.unwrap().unwrap().next_run.unwrap();
    assert!(next.timestamp_millis() >= before + 20 * 60_000, "{next}");

    // Back to normal: the far-off run is brought forward to one interval from now.
    let changed = now_ms();
    queue.set_pace(BackgroundPace::Normal).await.unwrap();
    let next = db.tasks().get("tick").await.unwrap().unwrap().next_run.unwrap();
    assert!(next.timestamp_millis() <= changed + 60_000 + 1_000, "{next}");
    assert_eq!(
        db.settings()
            .get::<BackgroundPace>(BACKGROUND_PACE_KEY)
            .await
            .unwrap(),
        Some(BackgroundPace::Normal)
    );

    cancel.cancel();
    handle.await.unwrap();
}
