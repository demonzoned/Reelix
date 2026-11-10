//! Download monitoring and completed/failed download handling ([`ActivityService`]).
//!
//! The `download_monitor` task polls every enabled client, keeps an in-memory snapshot of
//! progress for the queue API, starts tracking Reelix-category downloads it doesn't know yet,
//! imports completed downloads and handles failed ones (history, blocklist, re-search,
//! removal from the client).

mod content;
pub mod error;
pub mod jobs;
pub mod providers;
pub(crate) mod views;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use reelix_api_types::library::{LibrarySearchKind, SearchRequest};
use reelix_api_types::pipeline::{
    QueueBulkFailure, QueueBulkRemove, QueueBulkRemoveResult, QueueRemoveQuery,
};
use reelix_books::{BooksService, ContentDownload};
use reelix_core::bus::EventBus;
use reelix_core::{ContentKind, DomainEvent, MediaKind, Protocol};
use reelix_db::{Db, NewJob, now_ms};
use reelix_downloaders::{DownloadId, DownloadItem, DownloadState, FailureAction, UsenetFailure};
use reelix_import::files::TransferMode;
use reelix_import::matching::{best_movie_match, match_parsed_episodes};
use reelix_import::unpack::{UnpackConfig, remove_unpacked, unpack_download};
use reelix_jobs::JobQueue;
use reelix_library::LibraryService;
use reelix_music::MusicService;
use reelix_notify::{EpisodeSummary, FileSummary, MediaSummary, NotificationEvent, ReleaseSummary};
use reelix_parser::parse;

pub use error::{ActivityError, Result};
pub use providers::{ActivityProviders, ClientHandle, StaticProviders};

use crate::import::{FileImporter, Hint, ImportedItem, Target, transfer_mode};
use crate::store::blocklist::{self, NewBlocklist};
use crate::store::history::{self, HistoryEvent, NewHistory};
use crate::store::tracked::{self, NewTracked, TrackedDownload, TrackedState};
use crate::store::{ContentRef, FormatRef, GrabbedRelease};

/// Job kinds owned by the activity service.
pub mod kinds {
    /// Scheduled every 30 s; polls clients (every 2 min when nothing is active).
    pub const DOWNLOAD_MONITOR: &str = "download_monitor";
    /// Retry import of one queue item (`{"id": …}`).
    pub const PROCESS_DOWNLOAD: &str = "process_download";
    /// Payload: `ManualImportRequest`.
    pub const MANUAL_IMPORT: &str = "manual_import";
    /// Daily housekeeping: forget finished tracked downloads older than 30 days.
    pub const PRUNE_FINISHED: &str = "prune_finished";
}

const IDLE_POLL: Duration = Duration::from_secs(120);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(60);
const HEALTH_TTL: Duration = Duration::from_secs(24 * 3600);
/// Finished tracked rows are kept this long.
pub const PRUNE_AFTER_DAYS: i64 = 30;
/// An unknown download that finished longer ago than this is client history (often
/// imported by another tool sharing the category), not something for Reelix to import.
const ADOPT_FINISHED_WITHIN_SECS: i64 = 24 * 3600;

/// A trailer stored as a YouTube video key (or already a URL) as a watch URL.
pub(crate) fn youtube_url(key: &str) -> String {
    if key.starts_with("http") {
        key.to_owned()
    } else {
        format!("https://www.youtube.com/watch?v={key}")
    }
}

/// Codec and language facts of an imported file, for notifications.
fn file_facts(info: Option<&reelix_import::mediainfo::MediaInfo>) -> FileSummary {
    use reelix_import::naming::{audio_codec_name, dynamic_range_type, video_codec_name};
    let Some(info) = info else {
        return FileSummary::default();
    };
    let video = info.video_codec().map(|c| {
        let hdr = dynamic_range_type(info.hdr());
        let name = video_codec_name(c);
        if hdr.is_empty() {
            name.to_owned()
        } else {
            format!("{name} {hdr}")
        }
    });
    let audio = info.audio_codec().map(|c| {
        let atmos = info.primary_audio().is_some_and(|a| a.is_atmos);
        let name = audio_codec_name(c, atmos);
        match info.audio_channels() {
            Some(ch) => format!("{name} {ch:.1}"),
            None => name.to_owned(),
        }
    });
    let names = |langs: Vec<reelix_parser::Language>| {
        let mut out: Vec<String> = Vec::new();
        for l in langs
            .into_iter()
            .filter(|l| *l != reelix_parser::Language::Unknown)
        {
            let name = l.name().to_owned();
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    };
    FileSummary {
        video_codec: video,
        audio_codec: audio,
        audio_languages: names(info.audio_languages()),
        subtitle_languages: names(info.subtitle_languages()),
        ..Default::default()
    }
}

/// Whether an untracked download in a Reelix category should be tracked. Finished
/// items are only taken when they finished recently, or the client doesn't say when.
fn adoptable(item: &DownloadItem, now_secs: i64) -> bool {
    match item.state {
        DownloadState::Completed | DownloadState::Failed => item
            .completed_at
            .is_none_or(|at| now_secs - at <= ADOPT_FINISHED_WITHIN_SECS),
        _ => true,
    }
}

/// Live client state of one download (queue API).
#[derive(Debug, Clone, PartialEq)]
pub struct ItemSnapshot {
    pub client_name: String,
    pub state: DownloadState,
    pub progress: f64,
    pub total: u64,
    pub remaining: u64,
    pub eta_secs: Option<u64>,
    pub message: Option<String>,
}

impl ItemSnapshot {
    fn of(name: &str, i: &DownloadItem) -> Self {
        Self {
            client_name: name.to_owned(),
            state: i.state,
            progress: i.progress(),
            total: i.total_bytes,
            remaining: i.remaining_bytes,
            eta_secs: i.eta_secs,
            message: i.message.clone(),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
struct Snapshot {
    items: HashMap<(i64, String), ItemSnapshot>,
    /// client id → (name, error)
    client_errors: HashMap<i64, (String, String)>,
}

/// Release/download facts carried into history rows and notifications.
#[derive(Debug, Clone, Default)]
pub(crate) struct DownloadRef {
    pub title: String,
    pub download_id: Option<String>,
    pub protocol: Option<Protocol>,
    pub client: Option<String>,
    pub release: Option<GrabbedRelease>,
}

/// See the module docs.
pub struct ActivityService {
    pub(crate) db: Db,
    pub(crate) events: EventBus,
    pub(crate) library: Arc<LibraryService>,
    pub(crate) providers: Arc<dyn ActivityProviders>,
    snapshot: RwLock<Snapshot>,
    health: Mutex<Vec<(Instant, String)>>,
    busy: Mutex<HashSet<i64>>,
    /// (client id, download id) removed from a client during the current poll.
    removed: Mutex<HashSet<(i64, String)>>,
    dirty: AtomicBool,
    poll_lock: tokio::sync::Mutex<()>,
    last_poll: Mutex<Option<Instant>>,
    queue: OnceLock<JobQueue>,
    /// Book and album imports (set by the server after construction).
    pub(crate) books: OnceLock<Arc<BooksService>>,
    pub(crate) music: OnceLock<Arc<MusicService>>,
    /// Radio episode filing (set by the server after construction).
    pub(crate) radio: OnceLock<Arc<reelix_radio::RadioService>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Removes a tracked id from the busy set when an import finishes (or panics).
struct Busy<'a>(&'a Mutex<HashSet<i64>>, i64);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        lock(self.0).remove(&self.1);
    }
}

impl ActivityService {
    pub fn new(
        db: Db,
        events: EventBus,
        library: Arc<LibraryService>,
        providers: Arc<dyn ActivityProviders>,
    ) -> Self {
        Self {
            db,
            events,
            library,
            providers,
            snapshot: RwLock::default(),
            health: Mutex::default(),
            busy: Mutex::default(),
            removed: Mutex::default(),
            dirty: AtomicBool::new(false),
            poll_lock: tokio::sync::Mutex::new(()),
            last_poll: Mutex::default(),
            queue: OnceLock::new(),
            books: OnceLock::new(),
            music: OnceLock::new(),
            radio: OnceLock::new(),
        }
    }

    /// Gives the service the job queue (server `attach` phase).
    pub fn set_queue(&self, queue: JobQueue) {
        let _ = self.queue.set(queue);
    }

    /// Connects the books and music services (server build phase). Later calls are ignored.
    pub fn set_content(&self, books: Arc<BooksService>, music: Arc<MusicService>) {
        let _ = self.books.set(books);
        let _ = self.music.set(music);
    }

    /// Connects the radio service (server build phase). Later calls are ignored.
    pub fn set_radio(&self, radio: Arc<reelix_radio::RadioService>) {
        let _ = self.radio.set(radio);
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub(crate) fn emit(&self, entity: &str, action: &str, id: i64) {
        self.events.publish(DomainEvent::EntityChanged {
            entity: entity.to_owned(),
            action: action.to_owned(),
            id,
            user_id: None,
        });
    }

    pub(crate) async fn enqueue(&self, job: NewJob) -> Result<Option<i64>> {
        match self.queue.get() {
            Some(q) => Ok(q.enqueue(job).await?),
            None => {
                tracing::debug!(kind = %job.kind, "no job queue attached; job dropped");
                Ok(None)
            }
        }
    }

    pub(crate) fn snapshot_of(&self, client_id: i64, download_id: &str) -> Option<ItemSnapshot> {
        self.snapshot
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .items
            .get(&(client_id, download_id.to_owned()))
            .cloned()
    }

    /// Health findings: failed downloads that need attention (disk full, job too large) and
    /// clients that could not be polled.
    pub fn health_messages(&self) -> Vec<String> {
        let mut h = lock(&self.health);
        h.retain(|(t, _)| t.elapsed() < HEALTH_TTL);
        let mut out: Vec<String> = h.iter().map(|(_, m)| m.clone()).collect();
        let snap = self.snapshot.read().unwrap_or_else(|p| p.into_inner());
        let mut errs: Vec<_> = snap.client_errors.values().collect();
        errs.sort();
        for (name, e) in errs {
            out.push(format!("Unable to poll download client {name}: {e}"));
        }
        out
    }

    fn warn_health(&self, msg: String) {
        let mut h = lock(&self.health);
        h.retain(|(_, m)| *m != msg);
        h.push((Instant::now(), msg));
        if h.len() > 20 {
            h.remove(0);
        }
    }

    /// Updates state + messages when they differ (marks the queue dirty).
    async fn set_state(&self, t: &TrackedDownload, state: TrackedState, messages: Vec<String>) -> Result<()> {
        if t.state != state || t.status_messages != messages {
            tracked::set_state(&self.db, t.id, state, &messages).await?;
            self.dirty.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    pub(crate) fn client_name(&self, id: i64) -> Option<String> {
        self.providers.client(id).map(|c| c.name)
    }

    pub(crate) fn download_ref(&self, t: &TrackedDownload) -> DownloadRef {
        DownloadRef {
            title: t.title.clone(),
            download_id: Some(t.download_id.clone()),
            protocol: Some(t.protocol),
            client: self.client_name(t.download_client_id).or_else(|| {
                self.snapshot_of(t.download_client_id, &t.download_id)
                    .map(|s| s.client_name)
            }),
            release: t.release.clone(),
        }
    }

    // ------------------------------------------------------------------
    // Polling
    // ------------------------------------------------------------------

    /// Scheduled entry point: polls every run while downloads are active, else every 2 min.
    pub async fn monitor_tick(&self) -> Result<()> {
        let active = !tracked::active(&self.db).await?.is_empty();
        let due = lock(&self.last_poll).is_none_or(|t| t.elapsed() >= IDLE_POLL);
        if active || due {
            self.poll().await?;
        }
        Ok(())
    }

    /// Polls every client once and processes the results. Returns whether the queue changed.
    pub async fn poll(&self) -> Result<bool> {
        let _g = self.poll_lock.lock().await;
        *lock(&self.last_poll) = Some(Instant::now());
        lock(&self.removed).clear();
        let clients = self.providers.clients();
        let results = futures_join(&clients).await;

        let mut snap = Snapshot::default();
        let mut ok: HashMap<i64, (&ClientHandle, Vec<DownloadItem>)> = HashMap::new();
        for (h, r) in clients.iter().zip(results) {
            match r {
                Ok(items) => {
                    for i in &items {
                        snap.items
                            .insert((h.id, i.download_id.0.clone()), ItemSnapshot::of(&h.name, i));
                    }
                    ok.insert(h.id, (h, items));
                }
                Err(e) => {
                    tracing::warn!(client = %h.name, error = %e, "download client poll failed");
                    snap.client_errors.insert(h.id, (h.name.clone(), e));
                    // Keep the last known state of this client's downloads.
                    let old = self.snapshot.read().unwrap_or_else(|p| p.into_inner());
                    for (k, v) in old.items.iter().filter(|(k, _)| k.0 == h.id) {
                        snap.items.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        let changed_snapshot = {
            let mut cur = self.snapshot.write().unwrap_or_else(|p| p.into_inner());
            let changed = *cur != snap;
            *cur = snap;
            changed
        };

        // New Reelix-category downloads.
        let now_secs = now_ms() / 1000;
        for (h, items) in ok.values() {
            // slskd has no categories: its other downloads are the user's own, not ours.
            if h.client.protocol() == Protocol::Soulseek {
                continue;
            }
            for item in items.iter().filter(|i| adoptable(i, now_secs)) {
                if tracked::by_download(&self.db, h.id, &item.download_id.0)
                    .await?
                    .is_none()
                    && let Err(e) = self.track_unknown(h, item).await
                {
                    tracing::warn!(title = %item.title, error = %e, "could not track download");
                }
            }
        }

        // Active tracked downloads.
        for t in tracked::active(&self.db).await? {
            let Some((h, items)) = ok.get(&t.download_client_id) else {
                continue;
            };
            match items.iter().find(|i| i.download_id.0 == t.download_id) {
                Some(item) => {
                    if let Err(e) = self.process(&t, h, item).await {
                        tracing::warn!(id = t.id, title = %t.title, error = %e, "processing download failed");
                    }
                }
                None if t.state == TrackedState::Downloading => {
                    self.set_state(
                        &t,
                        TrackedState::Removed,
                        vec!["Removed from the download client".into()],
                    )
                    .await?;
                }
                None => {}
            }
        }

        // Imported torrents that were still seeding: remove once the client allows it.
        for t in tracked::with_state(&self.db, TrackedState::Imported).await? {
            let Some((h, items)) = ok.get(&t.download_client_id) else {
                continue;
            };
            if !h.client.info().remove_completed {
                continue;
            }
            if let Some(item) = items
                .iter()
                .find(|i| i.download_id.0 == t.download_id && i.can_be_removed)
                && !lock(&self.removed).contains(&(h.id, t.download_id.clone()))
            {
                self.remove_from_client(h, &item.download_id, true).await;
            }
        }

        let changed = changed_snapshot | self.dirty.swap(false, Ordering::Relaxed);
        if changed {
            self.emit("queue", "updated", 0);
        }
        Ok(changed)
    }

    async fn remove_from_client(&self, h: &ClientHandle, id: &DownloadId, delete_data: bool) {
        lock(&self.removed).insert((h.id, id.0.clone()));
        match h.client.remove(id, delete_data).await {
            Ok(()) => tracing::info!(client = %h.name, download = %id, "removed from download client"),
            Err(e) => {
                tracing::warn!(client = %h.name, download = %id, error = %e, "removing from client failed")
            }
        }
    }

    /// Starts tracking a download found in a Reelix category (grabbed outside Reelix or
    /// before its row existed): maps the title to library media when possible.
    async fn track_unknown(&self, h: &ClientHandle, item: &DownloadItem) -> Result<()> {
        let parsed = parse(&item.title);
        let content_kind = self.category_kind(h, item);
        let (movie_id, series_id, episode_ids) = if content_kind.is_some() {
            (None, None, Vec::new())
        } else {
            self.map_title(&parsed).await?
        };
        let content = match content_kind {
            Some(k) => self.map_content_title(k, &item.title).await,
            None => ContentRef::default(),
        };
        let release = GrabbedRelease {
            guid: format!("{}:{}", h.id, item.download_id),
            title: item.title.clone(),
            indexer_id: None,
            indexer: String::new(),
            protocol: h.client.protocol(),
            size: (item.total_bytes > 0).then_some(item.total_bytes),
            publish_date: None,
            download_url: None,
            magnet_url: None,
            info_url: None,
            info_hash: None,
            seeders: None,
            quality: parsed.quality,
            languages: parsed.languages.clone(),
            release_group: parsed.release_group.clone(),
            custom_formats: Vec::new(),
            format_score: 0,
            indexer_flags: Vec::new(),
            seed_ratio: None,
            seed_time_minutes: None,
            soulseek: None,
        };
        let id = tracked::insert(
            &self.db,
            &NewTracked {
                download_client_id: h.id,
                download_id: item.download_id.0.clone(),
                protocol: h.client.protocol(),
                title: item.title.clone(),
                movie_id,
                series_id,
                episode_ids,
                release,
            },
        )
        .await?;
        if !content.is_empty() {
            tracked::set_content(&self.db, id, &content).await?;
        } else if movie_id.is_none() && series_id.is_none() {
            let msg = match content_kind {
                Some(ContentKind::Music) => "Unknown download: not matched to an album in the library",
                Some(_) => "Unknown download: not matched to a book in the library",
                None => "Unknown download: not matched to a movie or series in the library",
            };
            tracked::set_state(&self.db, id, TrackedState::Downloading, &[msg.to_owned()]).await?;
        }
        tracing::info!(title = %item.title, client = %h.name, ?movie_id, ?series_id, ?content, "tracking download");
        self.dirty.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Ebook / music when the item sits in the client's book or music category (and that
    /// category isn't shared with movies or TV).
    fn category_kind(&self, h: &ClientHandle, item: &DownloadItem) -> Option<ContentKind> {
        let cat = item.category.as_deref()?.trim();
        if cat.is_empty() {
            return None;
        }
        let of = |k: ContentKind| h.client.category_for_kind(k);
        let video = [of(ContentKind::Movie), of(ContentKind::Series)];
        let is = |k: ContentKind| {
            of(k).is_some_and(|c| {
                c.eq_ignore_ascii_case(cat) && !video.iter().flatten().any(|v| v.eq_ignore_ascii_case(cat))
            })
        };
        if is(ContentKind::Ebook) && self.books.get().is_some() {
            Some(ContentKind::Ebook)
        } else if is(ContentKind::Music) && self.music.get().is_some() {
            Some(ContentKind::Music)
        } else {
            None
        }
    }

    /// Book (format) / album for a release title found in a book / music category.
    async fn map_content_title(&self, kind: ContentKind, title: &str) -> ContentRef {
        match kind {
            ContentKind::Music => {
                let Some(m) = self.music.get() else {
                    return ContentRef::default();
                };
                match m
                    .search_api()
                    .match_release(&reelix_parser::parse_music(title))
                    .await
                {
                    Ok(Some(id)) => ContentRef::album(id),
                    _ => ContentRef::default(),
                }
            }
            _ => {
                let Some(b) = self.books.get() else {
                    return ContentRef::default();
                };
                match b
                    .search_api()
                    .match_release(&reelix_parser::parse_book(title, None))
                    .await
                {
                    Ok(Some((id, f))) => ContentRef::book(id, f),
                    _ => ContentRef::default(),
                }
            }
        }
    }

    /// Movie / series (+ episodes) for a release title, by title (and year) against the library.
    pub(crate) async fn map_title(
        &self,
        parsed: &reelix_parser::ParsedRelease,
    ) -> Result<(Option<i64>, Option<i64>, Vec<i64>)> {
        if parsed.title.trim().is_empty() {
            return Ok((None, None, Vec::new()));
        }
        let candidates = self.library.importer().candidates().await?;
        let kind = if parsed.episode().is_some() {
            MediaKind::Series
        } else {
            MediaKind::Movie
        };
        let pool: Vec<_> = candidates.iter().filter(|c| c.kind == kind).collect();
        let expected: Vec<_> = pool.iter().map(|c| c.expected.clone()).collect();
        let mut q = parsed.clone();
        if kind == MediaKind::Series {
            q.year = None;
        }
        let Some((idx, _)) = best_movie_match(&q, &expected) else {
            return Ok((None, None, Vec::new()));
        };
        let id = pool[idx].id;
        match kind {
            MediaKind::Movie => Ok((Some(id), None, Vec::new())),
            MediaKind::Series => {
                let st = self.library.importer().series_target(id).await?;
                let eps = match_parsed_episodes(parsed, st.series_type, &st.expected())
                    .map(|m| m.episode_ids)
                    .unwrap_or_default();
                Ok((None, Some(id), eps))
            }
        }
    }

    async fn process(&self, t: &TrackedDownload, h: &ClientHandle, item: &DownloadItem) -> Result<()> {
        match (t.state, item.state) {
            (TrackedState::Importing, _) => {
                if !lock(&self.busy).contains(&t.id) {
                    // Interrupted import (restart): try again.
                    self.set_state(t, TrackedState::Downloading, Vec::new()).await?;
                }
                Ok(())
            }
            (TrackedState::ImportPending, _) => Ok(()),
            (_, DownloadState::Failed) => self.fail_from_client(t, h, item).await,
            (TrackedState::FailedPending, _) => Ok(()),
            (_, DownloadState::Completed) => self.complete(t, h, item).await,
            (_, DownloadState::Warning) => {
                let msg = item
                    .message
                    .clone()
                    .unwrap_or_else(|| "The download client reported a problem".into());
                self.set_state(t, TrackedState::Downloading, vec![msg]).await
            }
            _ => {
                // Clear transient messages once the download moves again (keep "unknown").
                if t.movie_id.is_some() || t.series_id.is_some() || !t.content.is_empty() {
                    self.set_state(t, TrackedState::Downloading, Vec::new()).await?;
                }
                Ok(())
            }
        }
    }

    /// Completed in the client: resolve the local path and import (or ask for manual import).
    async fn complete(&self, t: &TrackedDownload, h: &ClientHandle, item: &DownloadItem) -> Result<()> {
        let Some(remote) = item.output_path.as_deref().filter(|p| !p.trim().is_empty()) else {
            return self
                .set_state(
                    t,
                    TrackedState::Downloading,
                    vec!["The download client reported no output path".into()],
                )
                .await;
        };
        let local = self.providers.map_remote_path(&h.host, remote);
        let local_s = local.to_string_lossy().into_owned();
        if t.output_path.as_deref() != Some(local_s.as_str()) {
            tracked::set_output_path(&self.db, t.id, Some(&local_s)).await?;
        }
        if !local.exists() {
            return self
                .set_state(
                    t,
                    TrackedState::Downloading,
                    vec![format!(
                        "Output path {local_s} is not accessible; check remote path mappings"
                    )],
                )
                .await;
        }
        if t.movie_id.is_none() && t.series_id.is_none() && t.content.is_empty() {
            let msg = "Unknown download: it couldn't be matched to a movie or series. Use manual import.";
            self.needs_manual(t, vec![msg.to_owned()]).await?;
            return Ok(());
        }
        let mm = self.library.media_management().await?;
        let mode = transfer_mode(
            mm.import_mode,
            t.protocol == Protocol::Usenet,
            item.can_move_files,
        );
        if !t.content.is_empty() {
            self.import_content(t, &local, mode, Some((h, item))).await?;
            return Ok(());
        }
        self.import_tracked(t, &local, mode, Some((h, item))).await?;
        Ok(())
    }

    /// Marks a download as waiting for manual interaction (and notifies once).
    async fn needs_manual(&self, t: &TrackedDownload, messages: Vec<String>) -> Result<()> {
        let first = t.state != TrackedState::ImportPending;
        self.set_state(t, TrackedState::ImportPending, messages.clone())
            .await?;
        if first {
            let r = self.download_ref(t);
            let media = match (t.movie_id, t.series_id) {
                (None, None) => self.content_summary(&t.content).await,
                _ => Some(self.media_summary(t.movie_id, t.series_id, &t.episode_ids).await),
            };
            self.providers
                .notify(&NotificationEvent::ManualInteractionRequired {
                    media,
                    release: release_summary(&r),
                    message: messages.join("; "),
                })
                .await;
        }
        Ok(())
    }

    /// Imports a tracked download from `source`. Returns whether everything expected was imported.
    pub(crate) async fn import_tracked(
        &self,
        t: &TrackedDownload,
        source: &Path,
        mode: TransferMode,
        client: Option<(&ClientHandle, &DownloadItem)>,
    ) -> Result<bool> {
        if !lock(&self.busy).insert(t.id) {
            return Ok(false);
        }
        let _busy = Busy(&self.busy, t.id);
        tracked::set_state(&self.db, t.id, TrackedState::Importing, &[]).await?;
        let mut current = t.clone();
        current.state = TrackedState::Importing;
        current.status_messages.clear();
        self.emit("queue", "updated", t.id);
        match self.do_import(&current, source, mode).await {
            Ok((imported, messages)) => {
                let r = self.download_ref(t);
                for item in &imported {
                    self.record_import(item, &r).await;
                }
                let covered: HashSet<i64> = imported
                    .iter()
                    .flat_map(|i| i.target.episode_ids().iter().copied())
                    .collect();
                let complete = !imported.is_empty()
                    && (t.series_id.is_none() || t.episode_ids.iter().all(|e| covered.contains(e)));
                if complete {
                    self.set_state(&current, TrackedState::Imported, Vec::new())
                        .await?;
                    if let Some((h, item)) = client
                        && h.client.info().remove_completed
                        && item.can_be_removed
                    {
                        self.remove_from_client(h, &item.download_id, true).await;
                    }
                } else if imported.is_empty() {
                    let mut h = NewHistory::new(HistoryEvent::ImportFailed, &t.title);
                    fill_media(&mut h, t.movie_id, t.series_id, None);
                    fill_release(&mut h, &r);
                    h.data = serde_json::json!({ "messages": messages, "source_path": source });
                    history::insert(&self.db, &h).await?;
                    self.emit("history", "created", 0);
                    let msgs = if messages.is_empty() {
                        vec!["No video files were found that could be imported".to_owned()]
                    } else {
                        messages
                    };
                    self.needs_manual(&current, msgs).await?;
                } else {
                    let mut msgs = vec!["Not all expected episodes were imported".to_owned()];
                    msgs.extend(messages);
                    self.needs_manual(&current, msgs).await?;
                }
                self.dirty.store(true, Ordering::Relaxed);
                Ok(complete)
            }
            Err(e) => {
                tracing::warn!(id = t.id, error = %e, "import failed");
                self.needs_manual(&current, vec![format!("Import failed: {e}")])
                    .await?;
                Ok(false)
            }
        }
    }

    async fn do_import(
        &self,
        t: &TrackedDownload,
        source: &Path,
        mode: TransferMode,
    ) -> Result<(Vec<ImportedItem>, Vec<String>)> {
        let fi = FileImporter::new(&self.library).await?;
        let hint = Hint {
            movie_id: t.movie_id,
            series_id: if t.movie_id.is_some() { None } else { t.series_id },
            episode_ids: t.episode_ids.clone(),
            release_title: Some(t.title.clone()),
        };
        let mut planned = fi.plan(source, &hint).await?;
        let mut messages = Vec::new();
        // A download that holds only RAR archives: unpack them next to the archives and look again.
        let mut unpacked = Vec::new();
        if planned.is_empty() && source.is_dir() {
            let u = unpack_download(source, &UnpackConfig::default()).await;
            messages.extend(u.messages);
            unpacked = u.files;
            if !unpacked.is_empty() {
                planned = fi.plan(source, &hint).await?;
            }
        }
        let root = if source.is_dir() {
            source.to_path_buf()
        } else {
            source.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        let mut imported = Vec::new();
        for p in &planned {
            if p.accepted() {
                match fi.execute(p, mode, std::slice::from_ref(&root)).await {
                    Ok(i) => imported.push(i),
                    Err(e) => messages.push(format!("{}: {e}", p.relative.display())),
                }
            } else if !p.rejections.iter().all(|r| r == "Sample") {
                messages.push(format!("{}: {}", p.relative.display(), p.rejections.join("; ")));
            }
        }
        if planned.is_empty() && messages.is_empty() {
            messages.push("No video files found in the download".into());
        }
        // The library has its own copy or link now; the archives stay for the client to seed.
        if !unpacked.is_empty() && !imported.is_empty() && messages.is_empty() {
            remove_unpacked(&unpacked).await;
        }
        Ok((imported, messages))
    }

    /// History rows (one per episode), the import notification and a media server refresh.
    pub(crate) async fn record_import(&self, item: &ImportedItem, r: &DownloadRef) {
        let (movie_id, series_id) = match &item.target {
            Target::Movie(id) => (Some(*id), None),
            Target::Episodes { series_id, .. } => (None, Some(*series_id)),
        };
        let data = serde_json::json!({
            "source_path": item.source,
            "destination_path": item.destination,
            "is_upgrade": item.is_upgrade,
            "replaced": item.replaced,
            "extras": item.extras,
            "release_group": item.release_group,
        });
        let eps: Vec<Option<i64>> = if item.target.episode_ids().is_empty() {
            vec![None]
        } else {
            item.target.episode_ids().iter().map(|e| Some(*e)).collect()
        };
        for ep in eps {
            let mut h = NewHistory::new(HistoryEvent::Imported, &r.title);
            fill_media(&mut h, movie_id, series_id, ep);
            fill_release(&mut h, r);
            h.quality = Some(item.quality);
            h.languages = item.languages.clone();
            h.custom_formats = item
                .score
                .formats
                .iter()
                .map(|(id, name)| FormatRef {
                    id: *id,
                    name: name.clone(),
                })
                .collect();
            h.format_score = item.score.score;
            h.data = data.clone();
            if let Err(e) = history::insert(&self.db, &h).await {
                tracing::warn!(error = %e, "could not write import history");
            }
        }
        self.emit("history", "created", 0);

        let file = FileSummary {
            path: item.destination.to_string_lossy().into_owned(),
            size_bytes: Some(item.size),
            quality: Some(item.quality.quality.name().to_owned()),
            release_group: item.release_group.clone(),
            scene_name: item.scene_name.clone(),
            ..file_facts(item.media_info.as_ref())
        };
        let replaced_files = item
            .replaced
            .iter()
            .map(|p| FileSummary {
                path: p.to_string_lossy().into_owned(),
                ..Default::default()
            })
            .collect();
        let media = self
            .media_summary(movie_id, series_id, item.target.episode_ids())
            .await;
        self.providers
            .notify(&NotificationEvent::Import {
                media,
                release: Some(release_summary(r)),
                file,
                is_upgrade: item.is_upgrade,
                replaced_files,
            })
            .await;
        // Refresh the movie / series folder on media servers.
        let folder = match &item.target {
            Target::Movie(_) => item.destination.parent().map(Path::to_path_buf),
            Target::Episodes { series_id, .. } => match self.library.get_series(*series_id).await {
                Ok(s) => Some(PathBuf::from(s.path)),
                Err(_) => item.destination.parent().map(Path::to_path_buf),
            },
        };
        if let Some(f) = folder {
            self.providers.refresh_media_servers(&[f]).await;
        }
    }

    pub(crate) async fn media_summary(
        &self,
        movie_id: Option<i64>,
        series_id: Option<i64>,
        episode_ids: &[i64],
    ) -> MediaSummary {
        if let Some(id) = movie_id
            && let Ok(m) = self.library.get_movie(id).await
        {
            let mut s = MediaSummary::new(MediaKind::Movie, m.title);
            s.id = Some(m.id);
            s.year = m.year;
            s.tmdb_id = Some(i64::from(m.tmdb_id));
            s.imdb_id = m.imdb_id;
            s.poster_url = m.images.poster;
            s.backdrop_url = m.images.backdrop;
            s.overview = m.overview;
            s.genres = m.genres;
            s.rating = Some(m.ratings.vote_average).filter(|r| *r > 0.0);
            s.trailer_url = m.youtube_trailer.map(|k| youtube_url(&k));
            return s;
        }
        if let Some(id) = series_id
            && let Ok(m) = self.library.get_series(id).await
        {
            let mut s = MediaSummary::new(MediaKind::Series, m.title);
            s.id = Some(m.id);
            s.year = m.year;
            s.tmdb_id = m.tmdb_id.map(i64::from);
            s.tvdb_id = m.tvdb_id.map(i64::from);
            s.imdb_id = m.imdb_id;
            s.poster_url = m.images.poster;
            s.backdrop_url = m.images.backdrop;
            s.overview = m.overview;
            s.genres = m.genres;
            s.rating = Some(m.ratings.vote_average).filter(|r| *r > 0.0);
            if let Ok(eps) = self.library.importer().episodes(episode_ids).await {
                s.episodes = eps
                    .into_iter()
                    .map(|e| EpisodeSummary {
                        season: e.season_number,
                        episode: e.episode_number,
                        absolute: e.absolute_number,
                        title: e.title,
                        air_date: e.air_date.map(|d| d.format("%Y-%m-%d").to_string()),
                    })
                    .collect();
            }
            return s;
        }
        MediaSummary::new(
            if series_id.is_some() {
                MediaKind::Series
            } else {
                MediaKind::Movie
            },
            "Unknown",
        )
    }

    // ------------------------------------------------------------------
    // Failures
    // ------------------------------------------------------------------

    async fn fail_from_client(
        &self,
        t: &TrackedDownload,
        h: &ClientHandle,
        item: &DownloadItem,
    ) -> Result<()> {
        let msg = item
            .message
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "Download failed in the client".into());
        let failure = item.failure.unwrap_or_else(|| UsenetFailure::classify(&msg));
        self.handle_failure(t, &msg, failure.action(), Some(h), false)
            .await
    }

    /// History `download_failed`, blocklist / re-search / health warning per `action`, client
    /// removal (`remove_failed`), tracked → `failed`.
    async fn handle_failure(
        &self,
        t: &TrackedDownload,
        message: &str,
        action: FailureAction,
        client: Option<&ClientHandle>,
        skip_redownload: bool,
    ) -> Result<()> {
        tracked::set_state(&self.db, t.id, TrackedState::FailedPending, &[message.to_owned()]).await?;
        let r = self.download_ref(t);
        let eps: Vec<Option<i64>> = if t.episode_ids.is_empty() {
            vec![None]
        } else {
            t.episode_ids.iter().map(|e| Some(*e)).collect()
        };
        for ep in eps {
            let mut h = NewHistory::new(HistoryEvent::DownloadFailed, &t.title);
            fill_media(&mut h, t.movie_id, t.series_id, ep);
            self.fill_content(&mut h, &t.content).await;
            fill_release(&mut h, &r);
            h.data = serde_json::json!({ "message": message, "reason": action.reason });
            history::insert(&self.db, &h).await?;
        }
        self.emit("history", "created", 0);
        if action.blocklist {
            self.blocklist(t, action.reason).await?;
        }
        if action.search_again && !skip_redownload {
            self.search_again_for(t).await;
        }
        if action.health_warning {
            self.warn_health(format!("Download {} failed: {}", t.title, action.reason));
        }
        if let Some(h) = client
            && h.client.info().remove_failed
        {
            self.remove_from_client(h, &DownloadId(t.download_id.clone()), true)
                .await;
        }
        tracked::set_state(&self.db, t.id, TrackedState::Failed, &[message.to_owned()]).await?;
        tracing::info!(title = %t.title, reason = action.reason, "download failed");
        self.dirty.store(true, Ordering::Relaxed);
        self.emit("queue", "updated", t.id);
        Ok(())
    }

    async fn blocklist(&self, t: &TrackedDownload, reason: &str) -> Result<()> {
        let rel = t.release.as_ref();
        let id = blocklist::insert(
            &self.db,
            &NewBlocklist {
                movie_id: t.movie_id,
                series_id: t.series_id,
                episode_ids: t.episode_ids.clone(),
                source_title: t.title.clone(),
                quality: rel.map(|r| r.quality),
                languages: rel.map(|r| r.languages.clone()).unwrap_or_default(),
                protocol: Some(t.protocol),
                indexer: rel.map(|r| r.indexer.clone()).filter(|i| !i.is_empty()),
                size: rel.and_then(|r| r.size),
                publish_date: rel.and_then(|r| r.publish_date),
                torrent_info_hash: rel.and_then(GrabbedRelease::release_key).or_else(|| {
                    (t.protocol == Protocol::Torrent && t.download_id.len() == 40)
                        .then(|| t.download_id.clone())
                }),
                message: Some(reason.to_owned()),
                content: t.content,
            },
        )
        .await?;
        self.emit("blocklist", "created", id);
        Ok(())
    }

    /// Replacement search for whatever the download was for (movie, episodes, book, album).
    async fn search_again_for(&self, t: &TrackedDownload) {
        if t.content.is_empty() {
            self.search_again(t.movie_id, t.series_id, &t.episode_ids).await;
        } else {
            self.search_again_content(&t.content).await;
        }
    }

    /// Enqueues a replacement search (errors logged).
    async fn search_again(&self, movie_id: Option<i64>, series_id: Option<i64>, episode_ids: &[i64]) {
        let req = match (movie_id, series_id) {
            (Some(id), _) => SearchRequest {
                kind: LibrarySearchKind::Movie,
                ids: vec![id],
                season: None,
                media: None,
            },
            (None, Some(_)) if !episode_ids.is_empty() => SearchRequest {
                kind: LibrarySearchKind::Episode,
                ids: episode_ids.to_vec(),
                season: None,
                media: None,
            },
            (None, Some(id)) => SearchRequest {
                kind: LibrarySearchKind::Series,
                ids: vec![id],
                season: None,
                media: None,
            },
            (None, None) => return,
        };
        if let Err(e) = self.library.request_search(req).await {
            tracing::warn!(error = %e, "could not queue a replacement search");
        }
    }

    // ------------------------------------------------------------------
    // Queue / history actions
    // ------------------------------------------------------------------

    async fn tracked(&self, id: i64) -> Result<TrackedDownload> {
        tracked::get(&self.db, id)
            .await?
            .ok_or_else(|| ActivityError::not_found("queue item", id))
    }

    /// `DELETE /queue/{id}`.
    pub async fn remove(&self, id: i64, q: &QueueRemoveQuery) -> Result<()> {
        let t = self.tracked(id).await?;
        if q.remove_from_client {
            let h = self.providers.client(t.download_client_id).ok_or_else(|| {
                ActivityError::validation("the download client is not available (disabled or deleted)")
            })?;
            h.client
                .remove(&DownloadId(t.download_id.clone()), true)
                .await
                .map_err(|e| ActivityError::Other(format!("{}: {e}", h.name)))?;
        }
        if q.blocklist {
            self.blocklist(&t, "removed from queue by user").await?;
            if !q.skip_redownload {
                self.search_again_for(&t).await;
            }
        }
        tracked::set_state(
            &self.db,
            id,
            TrackedState::Removed,
            &["Removed from the queue".to_owned()],
        )
        .await?;
        self.snapshot
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .items
            .remove(&(t.download_client_id, t.download_id.clone()));
        self.emit("queue", "deleted", id);
        Ok(())
    }

    /// `DELETE /queue/bulk`: [`Self::remove`] for each id; one failure doesn't stop the rest.
    pub async fn remove_many(&self, req: &QueueBulkRemove) -> Result<QueueBulkRemoveResult> {
        const MAX: usize = 1000;
        if req.ids.len() > MAX {
            return Err(ActivityError::validation(format!(
                "remove at most {MAX} queue items at once"
            )));
        }
        let q = QueueRemoveQuery {
            remove_from_client: req.remove_from_client,
            blocklist: req.blocklist,
            skip_redownload: req.skip_redownload,
        };
        let mut out = QueueBulkRemoveResult::default();
        let mut seen = HashSet::new();
        for &id in req.ids.iter().filter(|id| seen.insert(**id)) {
            match self.remove(id, &q).await {
                Ok(()) => out.removed += 1,
                Err(e) => out.failed.push(QueueBulkFailure {
                    id,
                    error: e.to_string(),
                }),
            }
        }
        Ok(out)
    }

    /// `POST /queue/{id}/ignore`: stop tracking without importing.
    pub async fn ignore(&self, id: i64) -> Result<()> {
        let t = self.tracked(id).await?;
        tracked::set_state(&self.db, id, TrackedState::Ignored, &[]).await?;
        let mut h = NewHistory::new(HistoryEvent::Ignored, &t.title);
        fill_media(&mut h, t.movie_id, t.series_id, t.episode_ids.first().copied());
        self.fill_content(&mut h, &t.content).await;
        fill_release(&mut h, &self.download_ref(&t));
        history::insert(&self.db, &h).await?;
        self.emit("history", "created", 0);
        self.emit("queue", "deleted", id);
        Ok(())
    }

    /// `POST /queue/{id}/import`: queues a retry of the automatic import.
    pub async fn retry_import(&self, id: i64) -> Result<Option<i64>> {
        let t = self.tracked(id).await?;
        if t.state == TrackedState::Importing {
            return Err(ActivityError::Conflict("the download is being imported".into()));
        }
        if !t.state.is_active() {
            return Err(ActivityError::validation(
                "the download is no longer in the queue",
            ));
        }
        self.enqueue(
            NewJob::new(kinds::PROCESS_DOWNLOAD, serde_json::json!({ "id": id }))
                .dedupe(format!("process_download:{id}"))
                .priority(5),
        )
        .await
    }

    /// Job body of [`kinds::PROCESS_DOWNLOAD`]: re-checks the client and imports.
    pub async fn process_download(&self, id: i64) -> Result<bool> {
        let t = self.tracked(id).await?;
        if let Some(h) = self.providers.client(t.download_client_id)
            && let Ok(Ok(items)) = tokio::time::timeout(CLIENT_TIMEOUT, h.client.items()).await
            && let Some(item) = items.iter().find(|i| i.download_id.0 == t.download_id)
        {
            if item.state != DownloadState::Completed {
                return Err(ActivityError::validation("the download has not completed yet"));
            }
            // Reset so `complete` runs fresh (and re-notifies if it fails again).
            let mut fresh = t.clone();
            fresh.state = TrackedState::Downloading;
            self.complete(&fresh, &h, item).await?;
            let now = self.tracked(id).await?;
            self.emit("queue", "updated", id);
            return Ok(now.state == TrackedState::Imported);
        }
        let Some(path) = t.output_path.clone().filter(|p| Path::new(p).exists()) else {
            return Err(ActivityError::validation(
                "the download client doesn't report this download and its output path is unknown",
            ));
        };
        if t.movie_id.is_none() && t.series_id.is_none() && t.content.is_empty() {
            return Err(ActivityError::validation(
                "the download isn't matched to a movie or series; use manual import",
            ));
        }
        let mm = self.library.media_management().await?;
        let mode = transfer_mode(mm.import_mode, t.protocol == Protocol::Usenet, false);
        let ok = if t.content.is_empty() {
            self.import_tracked(&t, Path::new(&path), mode, None).await?
        } else {
            self.import_content(&t, Path::new(&path), mode, None).await?
        };
        self.emit("queue", "updated", id);
        Ok(ok)
    }

    /// `POST /history/{id}/failed`: blocklists the grabbed release and searches again.
    pub async fn mark_failed(&self, history_id: i64) -> Result<()> {
        let h = history::get(&self.db, history_id)
            .await?
            .ok_or_else(|| ActivityError::not_found("history item", history_id))?;
        if h.event != HistoryEvent::Grabbed {
            return Err(ActivityError::validation(
                "only grabbed releases can be marked as failed",
            ));
        }
        let action = FailureAction {
            blocklist: true,
            search_again: true,
            health_warning: false,
            reason: "marked as failed by user",
        };
        let tracked_row = match &h.download_id {
            Some(d) => tracked::by_download_id(&self.db, d).await?,
            None => None,
        };
        let t = match tracked_row {
            Some(t) => t,
            None => {
                // No tracked row (pruned): rebuild what failure handling needs from history.
                let grabs = match &h.download_id {
                    Some(d) => history::grabs_for_download(&self.db, d).await?,
                    None => vec![h.clone()],
                };
                TrackedDownload {
                    id: 0,
                    download_client_id: 0,
                    download_id: h.download_id.clone().unwrap_or_default(),
                    protocol: h.protocol.unwrap_or(Protocol::Torrent),
                    title: h.source_title.clone(),
                    movie_id: h.movie_id,
                    series_id: h.series_id,
                    episode_ids: grabs.iter().filter_map(|g| g.episode_id).collect(),
                    release: None,
                    content: h.content,
                    state: TrackedState::Imported,
                    status_messages: Vec::new(),
                    output_path: None,
                    added: h.date,
                    updated: h.date,
                }
            }
        };
        if t.id == 0 {
            let mut row = NewHistory::new(HistoryEvent::DownloadFailed, &t.title);
            fill_media(&mut row, t.movie_id, t.series_id, t.episode_ids.first().copied());
            row.content = h.content;
            row.author_id = h.author_id;
            row.artist_id = h.artist_id;
            row.download_id = h.download_id.clone();
            row.protocol = h.protocol;
            row.indexer = h.indexer.clone();
            row.download_client = h.download_client.clone();
            row.quality = h.quality;
            row.data = serde_json::json!({ "message": action.reason });
            history::insert(&self.db, &row).await?;
            let id = blocklist::insert(
                &self.db,
                &NewBlocklist {
                    movie_id: t.movie_id,
                    series_id: t.series_id,
                    episode_ids: t.episode_ids.clone(),
                    source_title: t.title.clone(),
                    quality: h.quality,
                    languages: h.languages.clone(),
                    protocol: h.protocol,
                    indexer: h.indexer.clone(),
                    message: Some(action.reason.into()),
                    content: t.content,
                    ..Default::default()
                },
            )
            .await?;
            self.emit("blocklist", "created", id);
            self.emit("history", "created", 0);
            self.search_again_for(&t).await;
            return Ok(());
        }
        self.handle_failure(&t, "Marked as failed by user", action, None, false)
            .await
    }

    /// Housekeeping: forget finished tracked rows older than [`PRUNE_AFTER_DAYS`].
    pub async fn prune(&self) -> Result<u64> {
        let cutoff = now_ms() - PRUNE_AFTER_DAYS * 86_400_000;
        Ok(tracked::prune_finished(&self.db, cutoff).await?)
    }
}

/// Polls every client concurrently (with a timeout). Errors become strings.
async fn futures_join(clients: &[ClientHandle]) -> Vec<std::result::Result<Vec<DownloadItem>, String>> {
    let mut set = tokio::task::JoinSet::new();
    for (i, c) in clients.iter().enumerate() {
        let client = c.client.clone();
        set.spawn(async move {
            let r = match tokio::time::timeout(CLIENT_TIMEOUT, client.items()).await {
                Ok(Ok(items)) => Ok(items),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err("timed out".to_owned()),
            };
            (i, r)
        });
    }
    let mut out: Vec<std::result::Result<Vec<DownloadItem>, String>> =
        (0..clients.len()).map(|_| Err("not polled".to_owned())).collect();
    while let Some(res) = set.join_next().await {
        if let Ok((i, r)) = res {
            out[i] = r;
        }
    }
    out
}

pub(crate) fn fill_media(
    h: &mut NewHistory,
    movie_id: Option<i64>,
    series_id: Option<i64>,
    episode: Option<i64>,
) {
    h.movie_id = movie_id;
    h.series_id = if movie_id.is_some() { None } else { series_id };
    h.episode_id = if movie_id.is_some() { None } else { episode };
}

pub(crate) fn fill_release(h: &mut NewHistory, r: &DownloadRef) {
    h.download_id = r.download_id.clone();
    h.protocol = r.protocol;
    h.download_client = r.client.clone();
    if let Some(rel) = &r.release {
        h.indexer = Some(rel.indexer.clone()).filter(|i| !i.is_empty());
        h.quality = Some(rel.quality);
        h.languages = rel.languages.clone();
        h.custom_formats = rel.custom_formats.clone();
        h.format_score = rel.format_score;
    }
}

pub(crate) fn release_summary(r: &DownloadRef) -> ReleaseSummary {
    let rel = r.release.as_ref();
    ReleaseSummary {
        title: r.title.clone(),
        indexer: rel.map(|x| x.indexer.clone()).filter(|i| !i.is_empty()),
        size_bytes: rel.and_then(|x| x.size),
        quality: rel.map(|x| x.quality.quality.name().to_owned()),
        protocol: r.protocol,
        release_group: rel.and_then(|x| x.release_group.clone()),
        custom_formats: rel
            .map(|x| x.custom_formats.iter().map(|f| f.name.clone()).collect())
            .unwrap_or_default(),
        custom_format_score: rel.map(|x| x.format_score),
        download_client: r.client.clone(),
        download_id: r.download_id.clone(),
    }
}
