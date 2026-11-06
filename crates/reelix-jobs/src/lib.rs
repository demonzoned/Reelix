//! Persistent job queue, worker pool and task scheduler.
//!
//! Jobs live in the `jobs` table (see `reelix-db`), so work survives restarts: jobs left
//! `running` by a crash are re-queued on startup. Each job runs in its own tokio task, so a
//! panicking handler only fails that job.

pub mod builtin;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use reelix_core::DomainEvent;
use reelix_core::bus::EventBus;
use reelix_core::jobs::{BACKGROUND_PACE_KEY, BackgroundPace, JobStatus};
use reelix_db::{Db, DbError, Job, NewJob, now_ms};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Executes jobs of one kind.
#[async_trait::async_trait]
pub trait JobHandler: Send + Sync + 'static {
    fn kind(&self) -> &'static str;
    /// Max concurrent jobs of this kind.
    fn concurrency(&self) -> usize {
        1
    }
    async fn run(&self, ctx: &JobContext, payload: serde_json::Value) -> anyhow::Result<()>;
}

/// What a running job gets access to. `cancel` fires on shutdown.
pub struct JobContext {
    pub db: Db,
    pub events: EventBus,
    pub job_id: i64,
    pub cancel: CancellationToken,
}

/// A recurring task; its job kind must have a registered handler.
#[derive(Debug, Clone)]
pub struct TaskDef {
    pub kind: &'static str,
    pub name: &'static str,
    pub interval: Duration,
    pub run_on_startup: bool,
}

struct QueueInner {
    db: Db,
    events: EventBus,
    notify: Notify,
    tasks: HashSet<&'static str>,
    /// How far scheduled task intervals are stretched (Settings, Background work).
    pace: RwLock<BackgroundPace>,
}

/// Handle for enqueueing jobs; cheap to clone.
#[derive(Clone)]
pub struct JobQueue {
    inner: Arc<QueueInner>,
}

impl JobQueue {
    /// Inserts the job, publishes `JobQueued` and wakes the runner. `None` if deduplicated.
    pub async fn enqueue(&self, job: NewJob) -> reelix_db::Result<Option<i64>> {
        let kind = job.kind.clone();
        let id = self.inner.db.jobs().enqueue(job).await?;
        if let Some(id) = id {
            self.inner.events.publish(DomainEvent::JobQueued { id, kind });
            self.inner.notify.notify_one();
        }
        Ok(id)
    }

    /// Enqueue a scheduled task's job now (dedupe key "task:{kind}"); Err(DbError::Conflict) if kind is unknown task.
    pub async fn run_task_now(&self, kind: &str) -> reelix_db::Result<Option<i64>> {
        if !self.inner.tasks.contains(kind) {
            return Err(DbError::Conflict(format!("unknown task {kind:?}")));
        }
        self.enqueue(task_job(kind)).await
    }

    fn wake(&self) {
        self.inner.notify.notify_one();
    }

    /// The current background work pace.
    pub fn pace(&self) -> BackgroundPace {
        *self.inner.pace.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Saves a new pace. A slower pace applies from each task's next run; a faster one also
    /// brings forward any task whose next run is now further away than its new interval.
    pub async fn set_pace(&self, pace: BackgroundPace) -> reelix_db::Result<()> {
        let db = &self.inner.db;
        db.settings().set(BACKGROUND_PACE_KEY, &pace).await?;
        *self.inner.pace.write().unwrap_or_else(|e| e.into_inner()) = pace;
        let now = now_ms();
        for task in db.tasks().list().await? {
            let soonest = now + pace.apply(task.interval_secs) * 1000;
            if task.next_run.is_some_and(|n| n.timestamp_millis() > soonest) {
                db.tasks().schedule_next(&task.kind, soonest).await?;
                self.inner
                    .events
                    .publish(DomainEvent::TaskUpdated { kind: task.kind });
            }
        }
        Ok(())
    }
}

fn task_job(kind: &str) -> NewJob {
    NewJob::new(kind, serde_json::json!({})).dedupe(format!("task:{kind}"))
}

struct Registered {
    handler: Arc<dyn JobHandler>,
    sem: Arc<Semaphore>,
    /// Also needs a `Shared::long_slots` permit (see [`JobSystemBuilder::long_running_handler`]).
    long_running: bool,
}

/// Global slots long-running jobs can't take, so the download monitor, RSS sync, searches and
/// imports keep running while library refreshes or scans take hours.
const RESERVED_SLOTS: usize = 2;

/// The permits a running job holds; released when it finishes.
struct Permits {
    _global: OwnedSemaphorePermit,
    _kind: OwnedSemaphorePermit,
    _long: Option<OwnedSemaphorePermit>,
}

struct Config {
    tick: Duration,
    poll: Duration,
    retry_base: Duration,
    shutdown_timeout: Duration,
}

struct Shared {
    queue: JobQueue,
    handlers: HashMap<&'static str, Registered>,
    kinds: Vec<&'static str>,
    global: Arc<Semaphore>,
    long_slots: Arc<Semaphore>,
    cfg: Config,
}

/// The job runner and scheduler. Build with [`JobSystem::builder`], then [`JobSystem::start`].
pub struct JobSystem {
    shared: Arc<Shared>,
    tasks: Vec<TaskDef>,
}

/// Builder for [`JobSystem`].
pub struct JobSystemBuilder {
    db: Db,
    events: EventBus,
    handlers: Vec<(Arc<dyn JobHandler>, bool)>,
    tasks: Vec<TaskDef>,
    global_concurrency: usize,
    cfg: Config,
}

impl JobSystemBuilder {
    pub fn handler(mut self, h: impl JobHandler) -> Self {
        self.handlers.push((Arc::new(h), false));
        self
    }

    /// Registers a handler whose jobs can run for minutes or hours (library refreshes, scans,
    /// renames, backups). Long-running jobs together never use more than the global limit minus
    /// two slots, so short jobs aren't starved.
    pub fn long_running_handler(mut self, h: impl JobHandler) -> Self {
        self.handlers.push((Arc::new(h), true));
        self
    }

    pub fn task(mut self, t: TaskDef) -> Self {
        self.tasks.push(t);
        self
    }

    /// Max jobs running at once across all kinds (default 4).
    pub fn global_concurrency(mut self, n: usize) -> Self {
        self.global_concurrency = n.max(1);
        self
    }

    /// Scheduler tick (default 5s).
    pub fn tick(mut self, d: Duration) -> Self {
        self.cfg.tick = d;
        self
    }

    /// How often the runner polls for delayed jobs without a wake-up (default 1s).
    pub fn poll_interval(mut self, d: Duration) -> Self {
        self.cfg.poll = d;
        self
    }

    /// Base retry delay; attempt `n` waits `base * 2^(n-1)`, capped at 1h (default 30s).
    pub fn retry_base(mut self, d: Duration) -> Self {
        self.cfg.retry_base = d;
        self
    }

    /// How long shutdown waits for in-flight jobs (default 30s).
    pub fn shutdown_timeout(mut self, d: Duration) -> Self {
        self.cfg.shutdown_timeout = d;
        self
    }

    pub fn build(self) -> JobSystem {
        let tasks: HashSet<&'static str> = self.tasks.iter().map(|t| t.kind).collect();
        let queue = JobQueue {
            inner: Arc::new(QueueInner {
                db: self.db,
                events: self.events,
                notify: Notify::new(),
                tasks,
                pace: RwLock::new(BackgroundPace::Normal),
            }),
        };
        let mut handlers = HashMap::new();
        for (h, long_running) in self.handlers {
            let sem = Arc::new(Semaphore::new(h.concurrency().max(1)));
            handlers.insert(
                h.kind(),
                Registered {
                    handler: h,
                    sem,
                    long_running,
                },
            );
        }
        let long_slots = self.global_concurrency.saturating_sub(RESERVED_SLOTS).max(1);
        let mut kinds: Vec<_> = handlers.keys().copied().collect();
        kinds.sort_unstable();
        JobSystem {
            shared: Arc::new(Shared {
                queue,
                handlers,
                kinds,
                global: Arc::new(Semaphore::new(self.global_concurrency)),
                long_slots: Arc::new(Semaphore::new(long_slots)),
                cfg: self.cfg,
            }),
            tasks: self.tasks,
        }
    }
}

impl JobSystem {
    pub fn builder(db: Db, events: EventBus) -> JobSystemBuilder {
        JobSystemBuilder {
            db,
            events,
            handlers: Vec::new(),
            tasks: Vec::new(),
            global_concurrency: 4,
            cfg: Config {
                tick: Duration::from_secs(5),
                poll: Duration::from_secs(1),
                retry_base: Duration::from_secs(30),
                shutdown_timeout: Duration::from_secs(30),
            },
        }
    }

    pub fn queue(&self) -> JobQueue {
        self.shared.queue.clone()
    }

    /// Requeues crashed jobs, ensures task rows, then runs the runner + scheduler until `cancel`;
    /// waits ≤30s for in-flight jobs.
    pub fn start(self, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let shared = self.shared;
            let db = &shared.queue.inner.db;
            match db.settings().get::<BackgroundPace>(BACKGROUND_PACE_KEY).await {
                Ok(Some(pace)) => {
                    *shared.queue.inner.pace.write().unwrap_or_else(|e| e.into_inner()) = pace;
                    if pace != BackgroundPace::Normal {
                        tracing::info!(?pace, "background work runs at a slower pace");
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "could not read the background work pace; using normal"),
            }
            match db.jobs().requeue_running().await {
                Ok(0) => {}
                Ok(n) => tracing::info!(count = n, "re-queued jobs interrupted by a previous shutdown"),
                Err(e) => tracing::error!(error = %e, "failed to re-queue interrupted jobs"),
            }
            for t in &self.tasks {
                if !shared.handlers.contains_key(t.kind) {
                    tracing::warn!(kind = t.kind, "scheduled task has no registered handler");
                }
                let now = now_ms();
                let interval_ms = interval_ms(t.interval);
                let first = if t.run_on_startup { now } else { now + interval_ms };
                let res = db.tasks().ensure(t.kind, t.name, interval_ms / 1000, first).await;
                // Tasks flagged run_on_startup run on every startup, not only the first.
                let res = match res {
                    Ok(()) if t.run_on_startup => db.tasks().schedule_next(t.kind, now).await,
                    other => other,
                };
                if let Err(e) = res {
                    tracing::error!(kind = t.kind, error = %e, "failed to register scheduled task");
                }
            }
            let runner = tokio::spawn(run_loop(shared.clone(), cancel.clone()));
            let scheduler = tokio::spawn(schedule_loop(shared.clone(), cancel.clone()));
            let _ = scheduler.await;
            let _ = runner.await;
            tracing::debug!("job system stopped");
        })
    }
}

fn interval_ms(d: Duration) -> i64 {
    (d.as_millis() as i64).max(1000)
}

/// `base * 2^(attempts-1)`, capped at one hour.
fn backoff(base: Duration, attempts: i64) -> Duration {
    let exp = (attempts - 1).clamp(0, 30) as u32;
    base.saturating_mul(1u32 << exp).min(Duration::from_secs(3600))
}

async fn run_loop(shared: Arc<Shared>, cancel: CancellationToken) {
    let tracker = TaskTracker::new();
    let rotation = AtomicUsize::new(0);
    while !cancel.is_cancelled() {
        dispatch(&shared, &tracker, &cancel, &rotation).await;
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = shared.queue.inner.notify.notified() => {}
            _ = tokio::time::sleep(shared.cfg.poll) => {}
        }
    }
    tracker.close();
    if !tracker.is_empty() {
        tracing::info!(count = tracker.len(), "waiting for running jobs to finish");
    }
    if tokio::time::timeout(shared.cfg.shutdown_timeout, tracker.wait())
        .await
        .is_err()
    {
        tracing::warn!(
            count = tracker.len(),
            "jobs still running at shutdown; they will be re-queued on restart"
        );
    }
}

/// Claims and spawns as many jobs as the concurrency limits allow.
async fn dispatch(
    shared: &Arc<Shared>,
    tracker: &TaskTracker,
    cancel: &CancellationToken,
    rot: &AtomicUsize,
) {
    let n = shared.kinds.len();
    if n == 0 {
        return;
    }
    // One read finds the kinds with runnable work, so an idle runner does not try a claim (a
    // database write) for every registered kind each poll.
    let ready = match shared.queue.inner.db.jobs().ready_kinds(now_ms()).await {
        Ok(ready) if ready.is_empty() => return,
        Ok(ready) => ready,
        Err(e) => {
            tracing::error!(error = %e, "failed to look for runnable jobs");
            return;
        }
    };
    // Rotate the starting kind so one busy kind can't monopolise the global slots.
    let start = rot.fetch_add(1, Ordering::Relaxed) % n;
    for i in 0..n {
        let kind = shared.kinds[(start + i) % n];
        if !ready.contains(kind) {
            continue;
        }
        let reg = &shared.handlers[kind];
        loop {
            if cancel.is_cancelled() {
                return;
            }
            let Ok(global) = shared.global.clone().try_acquire_owned() else {
                return;
            };
            let Ok(local) = reg.sem.clone().try_acquire_owned() else {
                break;
            };
            let long = if reg.long_running {
                match shared.long_slots.clone().try_acquire_owned() {
                    Ok(p) => Some(p),
                    Err(_) => break,
                }
            } else {
                None
            };
            match shared.queue.inner.db.jobs().claim(kind, now_ms()).await {
                Ok(Some(job)) => {
                    let shared = shared.clone();
                    let cancel = cancel.child_token();
                    let permits = Permits {
                        _global: global,
                        _kind: local,
                        _long: long,
                    };
                    tracker.spawn(execute(shared, job, permits, cancel));
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::error!(kind, error = %e, "failed to claim job");
                    break;
                }
            }
        }
    }
}

async fn execute(shared: Arc<Shared>, job: Job, permits: Permits, cancel: CancellationToken) {
    let inner = &shared.queue.inner;
    let kind = job.kind.clone();
    let handler = shared.handlers[kind.as_str()].handler.clone();
    inner.events.publish(DomainEvent::JobStarted {
        id: job.id,
        kind: kind.clone(),
    });
    tracing::debug!(id = job.id, kind, attempt = job.attempts, "job started");

    let ctx = JobContext {
        db: inner.db.clone(),
        events: inner.events.clone(),
        job_id: job.id,
        cancel,
    };
    let started = Instant::now();
    let payload = job.payload.clone();
    let joined = tokio::spawn(async move { handler.run(&ctx, payload).await }).await;
    let duration_ms = started.elapsed().as_millis() as i64;

    let outcome = match joined {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(e) if e.is_panic() => Err(format!("job panicked: {}", panic_message(e.into_panic()))),
        Err(_) => Err("job was cancelled".to_owned()),
    };
    let now = now_ms();
    let db = &inner.db;
    let (status, error, res) = match outcome {
        Ok(()) => (JobStatus::Completed, None, db.jobs().complete(job.id, now).await),
        Err(msg) if job.attempts < job.max_attempts => {
            let delay = backoff(shared.cfg.retry_base, job.attempts);
            tracing::warn!(id = job.id, kind, attempt = job.attempts, error = %msg, retry_in = ?delay, "job failed; will retry");
            let retry_at = now + delay.as_millis() as i64;
            let res = db.jobs().fail(job.id, now, &msg, Some(retry_at)).await;
            (JobStatus::Queued, Some(msg), res)
        }
        Err(msg) => {
            tracing::error!(id = job.id, kind, attempts = job.attempts, error = %msg, "job failed");
            let res = db.jobs().fail(job.id, now, &msg, None).await;
            (JobStatus::Failed, Some(msg), res)
        }
    };
    if let Err(e) = res {
        tracing::error!(id = job.id, kind, error = %e, "failed to record job result");
    }

    let is_task = inner.tasks.contains(kind.as_str());
    if is_task
        && let Err(e) = db
            .tasks()
            .record_run(&kind, now, duration_ms, error.as_deref())
            .await
    {
        tracing::error!(kind, error = %e, "failed to record task run");
    }
    inner.events.publish(DomainEvent::JobFinished {
        id: job.id,
        kind: kind.clone(),
        status,
        duration_ms,
        error,
    });
    if is_task {
        inner.events.publish(DomainEvent::TaskUpdated { kind });
    }

    drop(permits);
    shared.queue.wake();
}

fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

async fn schedule_loop(shared: Arc<Shared>, cancel: CancellationToken) {
    let mut ticker = tokio::time::interval(shared.cfg.tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => schedule_tick(&shared).await,
        }
    }
}

async fn schedule_tick(shared: &Shared) {
    let queue = &shared.queue;
    let db = &queue.inner.db;
    let now = now_ms();
    let pace = queue.pace();
    let due = match db.tasks().due(now).await {
        Ok(due) => due,
        Err(e) => {
            tracing::error!(error = %e, "failed to load due tasks");
            return;
        }
    };
    for task in due {
        if !shared.handlers.contains_key(task.kind.as_str()) {
            continue;
        }
        if let Err(e) = queue.enqueue(task_job(&task.kind)).await {
            tracing::error!(kind = task.kind, error = %e, "failed to enqueue scheduled task");
            continue;
        }
        let next = now + pace.apply(task.interval_secs) * 1000;
        match db.tasks().schedule_next(&task.kind, next).await {
            Ok(()) => queue
                .inner
                .events
                .publish(DomainEvent::TaskUpdated { kind: task.kind }),
            Err(e) => tracing::error!(kind = task.kind, error = %e, "failed to schedule next task run"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let base = Duration::from_secs(30);
        assert_eq!(backoff(base, 1), Duration::from_secs(30));
        assert_eq!(backoff(base, 2), Duration::from_secs(60));
        assert_eq!(backoff(base, 3), Duration::from_secs(120));
        assert_eq!(backoff(base, 10), Duration::from_secs(3600));
        assert_eq!(backoff(base, 1000), Duration::from_secs(3600));
    }
}
