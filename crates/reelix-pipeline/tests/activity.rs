//! Download monitoring, completed/failed handling, import and manual import against a fake
//! download client and temp directories.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use reelix_api_types::library::{ImportMode, MediaManagementSettings};
use reelix_api_types::pipeline::{
    HistoryQuery, ManualImportFile, ManualImportQuery, ManualImportRequest, QueueBulkRemove,
};
use reelix_core::bus::EventBus;
use reelix_core::{DomainEvent, Protocol};
use reelix_db::Db;
use reelix_downloaders::{
    AddOptions, ClientError, ClientInfo, ClientStatus, DownloadClient, DownloadId, DownloadItem,
    DownloadState, ReleasePayload, UsenetFailure,
};
use reelix_jobs::JobSystem;
use reelix_library::{DynMetadata, LibraryService};
use reelix_notify::NotificationEvent;
use reelix_parser::{Quality, QualityModel};
use reelix_pipeline::store::tracked::{self, NewTracked, TrackedState};
use reelix_pipeline::store::{blocklist, history};
use reelix_pipeline::{ActivityService, ClientHandle, GrabbedRelease, StaticProviders};

// ---------------------------------------------------------------- fakes

struct FakeClient {
    info: ClientInfo,
    protocol: Protocol,
    items: Mutex<Vec<DownloadItem>>,
    removed: Mutex<Vec<String>>,
}

#[async_trait]
impl DownloadClient for FakeClient {
    fn protocol(&self) -> Protocol {
        self.protocol
    }
    fn info(&self) -> &ClientInfo {
        &self.info
    }
    async fn add(&self, _r: &ReleasePayload, _o: &AddOptions) -> Result<DownloadId, ClientError> {
        Err(ClientError::Unsupported("add"))
    }
    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError> {
        Ok(self.items.lock().unwrap().clone())
    }
    async fn remove(&self, id: &DownloadId, _delete: bool) -> Result<(), ClientError> {
        self.removed.lock().unwrap().push(id.0.clone());
        self.items.lock().unwrap().retain(|i| i.download_id != *id);
        Ok(())
    }
    async fn test(&self) -> Result<(), ClientError> {
        Ok(())
    }
    async fn status(&self) -> Result<ClientStatus, ClientError> {
        Ok(ClientStatus::default())
    }
}

struct Env {
    db: Db,
    events: EventBus,
    svc: Arc<ActivityService>,
    client: Arc<FakeClient>,
    sent: Arc<Mutex<Vec<NotificationEvent>>>,
    movies: PathBuf,
    shows: PathBuf,
    downloads: PathBuf,
    _dir: tempfile::TempDir,
}

const CLIENT_ID: i64 = 7;

async fn env(protocol: Protocol, remove: bool) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let (movies, shows, downloads) = (
        dir.path().join("movies"),
        dir.path().join("tv"),
        dir.path().join("downloads"),
    );
    for d in [&movies, &shows, &downloads] {
        std::fs::create_dir_all(d).unwrap();
    }
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let jobs = JobSystem::builder(db.clone(), events.clone()).build();
    let library = Arc::new(LibraryService::new(
        db.clone(),
        events.clone(),
        Arc::new(DynMetadata::unconfigured()),
    ));
    library.set_queue(jobs.queue());
    library
        .set_media_management(MediaManagementSettings {
            analyze_video_files: false,
            minimum_free_space_mb: 0,
            recycle_bin_path: Some(dir.path().join("bin").to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
    let client = Arc::new(FakeClient {
        info: ClientInfo {
            implementation: "fake".into(),
            name: "Fake".into(),
            priority: 1,
            remove_completed: remove,
            remove_failed: remove,
            tags: vec![],
        },
        protocol,
        items: Mutex::default(),
        removed: Mutex::default(),
    });
    let providers = StaticProviders {
        clients: vec![ClientHandle {
            id: CLIENT_ID,
            name: "Fake".into(),
            host: "localhost".into(),
            client: client.clone(),
        }],
        roots: vec![downloads.clone()],
        ..Default::default()
    };
    let sent = providers.sent.clone();
    let svc = Arc::new(ActivityService::new(
        db.clone(),
        events.clone(),
        library,
        Arc::new(providers),
    ));
    svc.set_queue(jobs.queue());
    for (p, k) in [(&movies, "movie"), (&shows, "series")] {
        sqlx::query(
            "INSERT INTO root_folders (path, media_kind, created_at, updated_at) VALUES (?, ?, 0, 0)",
        )
        .bind(p.to_string_lossy().into_owned())
        .bind(k)
        .execute(db.pool())
        .await
        .unwrap();
    }
    Env {
        db,
        events,
        svc,
        client,
        sent,
        movies,
        shows,
        downloads,
        _dir: dir,
    }
}

async fn profile_id(db: &Db) -> i64 {
    db.quality_profiles().list().await.unwrap();
    sqlx::query_scalar("SELECT id FROM quality_profiles WHERE name = 'HD-1080p'")
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn add_movie(e: &Env) -> i64 {
    let profile = profile_id(&e.db).await;
    let root: i64 = sqlx::query_scalar("SELECT id FROM root_folders WHERE media_kind = 'movie'")
        .fetch_one(e.db.pool())
        .await
        .unwrap();
    sqlx::query_scalar(
        "INSERT INTO movies (tmdb_id, imdb_id, title, sort_title, clean_title, year, status, monitored, \
         minimum_availability, quality_profile_id, root_folder_id, path, created_at, updated_at) \
         VALUES (603, 'tt0133093', 'The Matrix', 'matrix', 'matrix', 1999, 'released', 1, 'released', ?, ?, ?, 0, 0) \
         RETURNING id",
    )
    .bind(profile)
    .bind(root)
    .bind(e.movies.join("The Matrix (1999)").to_string_lossy().into_owned())
    .fetch_one(e.db.pool())
    .await
    .unwrap()
}

/// "The Show" with S01E01–E03; returns (series id, episode ids).
async fn add_series(e: &Env) -> (i64, Vec<i64>) {
    let profile = profile_id(&e.db).await;
    let root: i64 = sqlx::query_scalar("SELECT id FROM root_folders WHERE media_kind = 'series'")
        .fetch_one(e.db.pool())
        .await
        .unwrap();
    let sid: i64 = sqlx::query_scalar(
        "INSERT INTO series (tmdb_id, tvdb_id, title, sort_title, clean_title, year, quality_profile_id, \
         root_folder_id, path, created_at, updated_at) \
         VALUES (1399, 121361, 'The Show', 'show', 'show', 2020, ?, ?, ?, 0, 0) RETURNING id",
    )
    .bind(profile)
    .bind(root)
    .bind(e.shows.join("The Show (2020)").to_string_lossy().into_owned())
    .fetch_one(e.db.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO seasons (series_id, season_number, monitored) VALUES (?, 1, 1)")
        .bind(sid)
        .execute(e.db.pool())
        .await
        .unwrap();
    let mut ids = Vec::new();
    for n in 1..=3 {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO episodes (series_id, season_number, episode_number, title, air_date, monitored, created_at, \
             updated_at) VALUES (?, 1, ?, ?, ?, 1, 0, 0) RETURNING id",
        )
        .bind(sid)
        .bind(n)
        .bind(format!("Episode {n}"))
        .bind(format!("2020-01-0{n}"))
        .fetch_one(e.db.pool())
        .await
        .unwrap();
        ids.push(id);
    }
    (sid, ids)
}

fn release(title: &str, protocol: Protocol, quality: Quality) -> GrabbedRelease {
    GrabbedRelease {
        guid: format!("guid-{title}"),
        title: title.into(),
        indexer_id: Some(1),
        indexer: "Idx".into(),
        protocol,
        size: Some(1000),
        publish_date: None,
        download_url: Some("http://x/nzb".into()),
        magnet_url: None,
        info_url: None,
        info_hash: None,
        seeders: None,
        quality: QualityModel::new(quality),
        languages: vec![],
        release_group: Some("GRP".into()),
        custom_formats: vec![],
        format_score: 0,
        indexer_flags: vec![],
        seed_ratio: None,
        seed_time_minutes: None,
        soulseek: None,
    }
}

async fn track(
    e: &Env,
    id: &str,
    title: &str,
    movie: Option<i64>,
    series: Option<i64>,
    eps: Vec<i64>,
) -> i64 {
    let protocol = e.client.protocol;
    tracked::insert(
        &e.db,
        &NewTracked {
            download_client_id: CLIENT_ID,
            download_id: id.into(),
            protocol,
            title: title.into(),
            movie_id: movie,
            series_id: series,
            episode_ids: eps,
            release: release(title, protocol, Quality::Bluray1080p),
        },
    )
    .await
    .unwrap()
}

fn item(id: &str, title: &str, state: DownloadState, output: Option<&Path>) -> DownloadItem {
    let mut i = DownloadItem::new(id, title, state);
    i.total_bytes = 1000;
    i.remaining_bytes = if state == DownloadState::Completed { 0 } else { 400 };
    i.output_path = output.map(|p| p.to_string_lossy().into_owned());
    i.can_be_removed = true;
    i.can_move_files = true;
    i
}

fn write(p: &Path, bytes: usize) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, vec![b'x'; bytes]).unwrap();
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(files_under(&p));
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

async fn state_of(e: &Env, id: i64) -> TrackedState {
    tracked::get(&e.db, id).await.unwrap().unwrap().state
}

async fn history_events(e: &Env) -> Vec<String> {
    e.svc
        .history(&HistoryQuery::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|h| h.event_type)
        .collect()
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn monitor_tracks_category_downloads_and_reports_progress() {
    let e = env(Protocol::Torrent, false).await;
    let movie = add_movie(&e).await;
    let mut rx = e.events.subscribe();
    e.client.items.lock().unwrap().extend([
        item(
            "hash1",
            "The.Matrix.1999.1080p.BluRay.x264-GRP",
            DownloadState::Downloading,
            None,
        ),
        item(
            "hash2",
            "Totally.Unrelated.Thing.2011.720p",
            DownloadState::Downloading,
            None,
        ),
    ]);
    assert!(e.svc.poll().await.unwrap());
    let q = e.svc.queue(None, None).await.unwrap();
    assert_eq!(q.total, 2);
    let matrix = q.items.iter().find(|i| i.download_id == "hash1").unwrap();
    assert_eq!(matrix.media.as_ref().unwrap().id, movie);
    assert_eq!(matrix.media.as_ref().unwrap().title, "The Matrix");
    assert_eq!(matrix.client_state.as_deref(), Some("downloading"));
    assert!((matrix.progress - 0.6).abs() < 1e-9);
    assert_eq!(matrix.download_client, "Fake");
    let unknown = q.items.iter().find(|i| i.download_id == "hash2").unwrap();
    assert!(unknown.media.is_none());
    assert!(unknown.messages[0].contains("Unknown download"));
    let mut saw_queue = false;
    while let Ok(ev) = rx.try_recv() {
        if matches!(&ev, DomainEvent::EntityChanged { entity, .. } if entity == "queue") {
            saw_queue = true;
        }
    }
    assert!(saw_queue);

    // Nothing changed → no update event.
    assert!(!e.svc.poll().await.unwrap());

    // Gone from the client while downloading → removed from the queue.
    e.client
        .items
        .lock()
        .unwrap()
        .retain(|i| i.download_id.0 == "hash1");
    assert!(e.svc.poll().await.unwrap());
    assert_eq!(e.svc.queue(None, None).await.unwrap().total, 1);
}

#[tokio::test]
async fn completed_torrent_movie_is_hardlinked_into_the_library() {
    let e = env(Protocol::Torrent, false).await;
    let movie = add_movie(&e).await;
    let title = "The.Matrix.1999.1080p.BluRay.x264-GRP";
    let id = track(&e, "hash1", title, Some(movie), None, vec![]).await;
    let folder = e.downloads.join(title);
    write(&folder.join(format!("{title}.mkv")), 5000);
    write(&folder.join(format!("{title}.en.srt")), 10);
    write(&folder.join("sample").join("sample.mkv"), 100);
    e.client
        .items
        .lock()
        .unwrap()
        .push(item("hash1", title, DownloadState::Completed, Some(&folder)));

    e.svc.poll().await.unwrap();
    assert_eq!(state_of(&e, id).await, TrackedState::Imported);
    let lib = files_under(&e.movies.join("The Matrix (1999)"));
    assert_eq!(lib.len(), 2, "{lib:?}");
    let video = lib.iter().find(|p| p.extension().unwrap() == "mkv").unwrap();
    assert!(
        video
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("The Matrix (1999)")
    );
    assert!(lib.iter().any(|p| p.to_string_lossy().ends_with(".en.srt")));
    // Hardlinked/copied: the seeding source stays.
    assert!(folder.join(format!("{title}.mkv")).exists());
    let (rel, quality): (String, String) =
        sqlx::query_as("SELECT relative_path, quality FROM movie_files WHERE movie_id = ?")
            .bind(movie)
            .fetch_one(e.db.pool())
            .await
            .unwrap();
    assert!(rel.ends_with(".mkv"));
    assert!(quality.contains(&Quality::Bluray1080p.id().to_string()) || quality.contains("Bluray"));
    assert_eq!(history_events(&e).await, vec!["imported"]);
    assert!(e.client.removed.lock().unwrap().is_empty());
    assert!(e.sent.lock().unwrap().iter().any(|n| matches!(
        n,
        NotificationEvent::Import {
            is_upgrade: false,
            ..
        }
    )));
    // Queue no longer lists it.
    assert_eq!(e.svc.queue(None, None).await.unwrap().total, 0);
}

#[tokio::test]
async fn completed_usenet_movie_is_moved_and_removed_from_client() {
    let e = env(Protocol::Usenet, true).await;
    let movie = add_movie(&e).await;
    let title = "The.Matrix.1999.720p.HDTV.x264-GRP";
    let id = track(&e, "nzo1", title, Some(movie), None, vec![]).await;
    let folder = e.downloads.join(title);
    let src = folder.join("abc123.mkv");
    write(&src, 5000);
    e.client
        .items
        .lock()
        .unwrap()
        .push(item("nzo1", title, DownloadState::Completed, Some(&folder)));
    e.svc.poll().await.unwrap();
    assert_eq!(state_of(&e, id).await, TrackedState::Imported);
    assert!(!src.exists(), "usenet imports move");
    assert_eq!(files_under(&e.movies.join("The Matrix (1999)")).len(), 1);
    assert_eq!(*e.client.removed.lock().unwrap(), vec!["nzo1".to_owned()]);
}

#[tokio::test]
async fn upgrade_replaces_old_file_into_recycle_bin() {
    let e = env(Protocol::Usenet, false).await;
    let movie = add_movie(&e).await;
    // First: an HDTV-720p release.
    let t1 = "The.Matrix.1999.720p.HDTV.x264-OLD";
    let id1 = track(&e, "nzo1", t1, Some(movie), None, vec![]).await;
    write(&e.downloads.join(t1).join(format!("{t1}.mkv")), 3000);
    e.client.items.lock().unwrap().push(item(
        "nzo1",
        t1,
        DownloadState::Completed,
        Some(&e.downloads.join(t1)),
    ));
    e.svc.poll().await.unwrap();
    assert_eq!(state_of(&e, id1).await, TrackedState::Imported);
    let first = files_under(&e.movies.join("The Matrix (1999)"));
    assert_eq!(first.len(), 1);

    // Then a Bluray-1080p upgrade.
    let t2 = "The.Matrix.1999.1080p.BluRay.x264-NEW";
    let id2 = track(&e, "nzo2", t2, Some(movie), None, vec![]).await;
    write(&e.downloads.join(t2).join(format!("{t2}.mkv")), 6000);
    e.client.items.lock().unwrap().push(item(
        "nzo2",
        t2,
        DownloadState::Completed,
        Some(&e.downloads.join(t2)),
    ));
    e.svc.poll().await.unwrap();
    assert_eq!(state_of(&e, id2).await, TrackedState::Imported);
    let now = files_under(&e.movies.join("The Matrix (1999)"));
    assert_eq!(now.len(), 1);
    assert!(now[0].to_string_lossy().contains("NEW"), "{now:?}");
    let bin = files_under(&e._dir.path().join("bin"));
    assert_eq!(bin.len(), 1, "old file recycled: {bin:?}");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM movie_files WHERE movie_id = ?")
        .bind(movie)
        .fetch_one(e.db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1);
    let h = e.svc.history(&HistoryQuery::default()).await.unwrap();
    assert_eq!(h.items[0].data["is_upgrade"], true);

    // A worse release afterwards is rejected → waits for manual interaction.
    let t3 = "The.Matrix.1999.480p.DVDRip.x264-BAD";
    let id3 = track(&e, "nzo3", t3, Some(movie), None, vec![]).await;
    write(&e.downloads.join(t3).join(format!("{t3}.mkv")), 2000);
    e.client.items.lock().unwrap().push(item(
        "nzo3",
        t3,
        DownloadState::Completed,
        Some(&e.downloads.join(t3)),
    ));
    e.svc.poll().await.unwrap();
    let t = tracked::get(&e.db, id3).await.unwrap().unwrap();
    assert_eq!(t.state, TrackedState::ImportPending);
    assert!(
        t.status_messages.iter().any(|m| m.contains("Not an upgrade")),
        "{:?}",
        t.status_messages
    );
    assert!(
        e.sent
            .lock()
            .unwrap()
            .iter()
            .any(|n| matches!(n, NotificationEvent::ManualInteractionRequired { .. }))
    );
}

#[tokio::test]
async fn season_pack_maps_files_to_episodes() {
    let e = env(Protocol::Torrent, false).await;
    let (sid, eps) = add_series(&e).await;
    let title = "The.Show.S01.1080p.WEB-DL.x264-GRP";
    let id = track(&e, "hashS", title, None, Some(sid), eps.clone()).await;
    let folder = e.downloads.join(title);
    for n in 1..=3 {
        write(
            &folder.join(format!("The.Show.S01E0{n}.1080p.WEB-DL.x264-GRP.mkv")),
            4000 + n,
        );
    }
    write(&folder.join("The.Show.S01E01.sample.mkv"), 10);
    e.client
        .items
        .lock()
        .unwrap()
        .push(item("hashS", title, DownloadState::Completed, Some(&folder)));
    e.svc.poll().await.unwrap();
    assert_eq!(state_of(&e, id).await, TrackedState::Imported);
    let lib = files_under(&e.shows.join("The Show (2020)").join("Season 01"));
    assert_eq!(lib.len(), 3, "{lib:?}");
    let linked: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT episode_file_id) FROM episodes WHERE series_id = ? AND episode_file_id IS NOT NULL",
    )
    .bind(sid)
    .fetch_one(e.db.pool())
    .await
    .unwrap();
    assert_eq!(linked, 3);
    let h = e.svc.history(&HistoryQuery::default()).await.unwrap();
    assert_eq!(h.total, 3);
    assert!(
        h.items
            .iter()
            .all(|r| r.event_type == "imported" && r.episode.is_some())
    );
    assert_eq!(h.items[0].media.as_ref().unwrap().kind, "series");
}

#[tokio::test]
async fn failed_usenet_download_blocklists_and_searches_again() {
    let e = env(Protocol::Usenet, true).await;
    let movie = add_movie(&e).await;
    let title = "The.Matrix.1999.1080p.BluRay.x264-GRP";
    let id = track(&e, "nzo1", title, Some(movie), None, vec![]).await;
    let mut it = item("nzo1", title, DownloadState::Failed, None);
    it.message = Some("Aborted, cannot be completed - missing articles".into());
    it.failure = Some(UsenetFailure::MissingArticles);
    e.client.items.lock().unwrap().push(it);
    e.svc.poll().await.unwrap();
    assert_eq!(state_of(&e, id).await, TrackedState::Failed);
    let (bl, total) = blocklist::list(&e.db, 0, 10).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(bl[0].source_title, title);
    assert_eq!(bl[0].message.as_deref(), Some("missing articles"));
    assert_eq!(history_events(&e).await, vec!["download_failed"]);
    let jobs: Vec<String> = sqlx::query_scalar("SELECT kind FROM jobs")
        .fetch_all(e.db.pool())
        .await
        .unwrap();
    assert!(jobs.contains(&"search".to_owned()), "{jobs:?}");
    assert_eq!(*e.client.removed.lock().unwrap(), vec!["nzo1".to_owned()]);

    // Disk full: no blocklist, health warning instead.
    let id2 = track(
        &e,
        "nzo2",
        "The.Matrix.1999.720p.HDTV-X",
        Some(movie),
        None,
        vec![],
    )
    .await;
    let mut it = item("nzo2", "The.Matrix.1999.720p.HDTV-X", DownloadState::Failed, None);
    it.message = Some("Unpacking failed, write error or disk is full?".into());
    e.client.items.lock().unwrap().push(it);
    e.svc.poll().await.unwrap();
    assert_eq!(state_of(&e, id2).await, TrackedState::Failed);
    assert_eq!(blocklist::list(&e.db, 0, 10).await.unwrap().1, 1);
    assert!(
        e.svc
            .health_messages()
            .iter()
            .any(|m| m.contains("out of disk space"))
    );
}

#[tokio::test]
async fn mark_grabbed_history_as_failed() {
    let e = env(Protocol::Usenet, false).await;
    let movie = add_movie(&e).await;
    let title = "The.Matrix.1999.1080p.BluRay.x264-GRP";
    let id = track(&e, "nzo1", title, Some(movie), None, vec![]).await;
    let mut h = history::NewHistory::new(history::HistoryEvent::Grabbed, title);
    h.movie_id = Some(movie);
    h.download_id = Some("nzo1".into());
    let hid = history::insert(&e.db, &h).await.unwrap();
    assert!(e.svc.mark_failed(hid).await.is_ok());
    assert_eq!(state_of(&e, id).await, TrackedState::Failed);
    assert_eq!(blocklist::list(&e.db, 0, 10).await.unwrap().1, 1);
    // Only grabbed rows can be marked.
    let rows = e.svc.history(&HistoryQuery::default()).await.unwrap();
    let failed = rows
        .items
        .iter()
        .find(|r| r.event_type == "download_failed")
        .unwrap();
    assert!(e.svc.mark_failed(failed.id).await.is_err());
}

#[tokio::test]
async fn unknown_download_waits_for_manual_import() {
    let e = env(Protocol::Torrent, false).await;
    let movie = add_movie(&e).await;
    let title = "Some.Obscure.Release.2020.1080p.WEB-DL-XYZ";
    let folder = e.downloads.join(title);
    let src = folder.join("video.mkv");
    write(&src, 5000);
    e.client
        .items
        .lock()
        .unwrap()
        .push(item("hashU", title, DownloadState::Completed, Some(&folder)));
    e.svc.poll().await.unwrap();
    let q = e.svc.queue(None, None).await.unwrap();
    assert_eq!(q.items.len(), 1);
    let qi = &q.items[0];
    assert_eq!(qi.state, "import_pending");
    assert!(qi.can_manual_import);

    // Preview by download id: the file is listed without media.
    let preview = e
        .svc
        .manual_import_preview(&ManualImportQuery {
            download_id: Some("hashU".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(preview.len(), 1);
    assert!(preview[0].media.is_none());
    assert!(!preview[0].rejections.is_empty());
    // Restricting to the movie maps it.
    let preview = e
        .svc
        .manual_import_preview(&ManualImportQuery {
            download_id: Some("hashU".into()),
            movie_id: Some(movie),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(preview[0].media.as_ref().unwrap().id, movie);

    let report = e
        .svc
        .manual_import(&ManualImportRequest {
            files: vec![ManualImportFile {
                path: src.to_string_lossy().into_owned(),
                movie_id: Some(movie),
                series_id: None,
                episode_ids: vec![],
                quality: Some(QualityModel::new(Quality::WebDl1080p)),
                languages: vec![],
                release_group: None,
                download_id: Some("hashU".into()),
            }],
            import_mode: "copy".into(),
        })
        .await
        .unwrap();
    assert_eq!(report.imported, 1, "{:?}", report.failures);
    assert!(src.exists(), "copy keeps the source");
    assert_eq!(files_under(&e.movies.join("The Matrix (1999)")).len(), 1);
    let t = tracked::by_download_id(&e.db, "hashU").await.unwrap().unwrap();
    assert_eq!(t.state, TrackedState::Imported);
    assert_eq!(t.movie_id, Some(movie));
    assert_eq!(history_events(&e).await, vec!["imported"]);
}

#[tokio::test]
async fn manual_import_folder_must_be_allowed() {
    let e = env(Protocol::Torrent, false).await;
    add_movie(&e).await;
    let outside = tempfile::tempdir().unwrap();
    let err = e
        .svc
        .manual_import_preview(&ManualImportQuery {
            folder: Some(outside.path().to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await;
    assert!(err.is_err());
    // A download folder is fine and maps by title.
    let folder = e.downloads.join("The.Matrix.1999.1080p.BluRay.x264-GRP");
    write(&folder.join("The.Matrix.1999.1080p.BluRay.x264-GRP.mkv"), 5000);
    let items = e
        .svc
        .manual_import_preview(&ManualImportQuery {
            folder: Some(e.downloads.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].media.as_ref().unwrap().title, "The Matrix");
    assert!(items[0].rejections.is_empty(), "{:?}", items[0].rejections);
}

#[tokio::test]
async fn queue_actions_remove_ignore_and_prune() {
    let e = env(Protocol::Torrent, false).await;
    let movie = add_movie(&e).await;
    let a = track(
        &e,
        "a",
        "The.Matrix.1999.1080p.BluRay-A",
        Some(movie),
        None,
        vec![],
    )
    .await;
    let b = track(
        &e,
        "b",
        "The.Matrix.1999.1080p.BluRay-B",
        Some(movie),
        None,
        vec![],
    )
    .await;
    e.client.items.lock().unwrap().push(item(
        "a",
        "The.Matrix.1999.1080p.BluRay-A",
        DownloadState::Downloading,
        None,
    ));
    e.svc
        .remove(
            a,
            &reelix_api_types::pipeline::QueueRemoveQuery {
                remove_from_client: true,
                blocklist: true,
                skip_redownload: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(state_of(&e, a).await, TrackedState::Removed);
    assert_eq!(*e.client.removed.lock().unwrap(), vec!["a".to_owned()]);
    assert_eq!(blocklist::list(&e.db, 0, 10).await.unwrap().1, 1);
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE kind = 'search'")
        .fetch_one(e.db.pool())
        .await
        .unwrap();
    assert_eq!(jobs, 0, "skip_redownload");

    e.svc.ignore(b).await.unwrap();
    assert_eq!(state_of(&e, b).await, TrackedState::Ignored);
    assert_eq!(history_events(&e).await, vec!["ignored"]);
    assert_eq!(e.svc.queue(None, None).await.unwrap().total, 0);
    assert!(e.svc.retry_import(b).await.is_err());
    // Nothing is old enough to prune yet.
    assert_eq!(e.svc.prune().await.unwrap(), 0);
}

#[tokio::test]
async fn retry_import_after_fixing_the_problem() {
    let e = env(Protocol::Usenet, false).await;
    let movie = add_movie(&e).await;
    let title = "The.Matrix.1999.1080p.BluRay.x264-GRP";
    let id = track(&e, "nzo1", title, Some(movie), None, vec![]).await;
    let folder = e.downloads.join(title);
    std::fs::create_dir_all(&folder).unwrap();
    e.client
        .items
        .lock()
        .unwrap()
        .push(item("nzo1", title, DownloadState::Completed, Some(&folder)));
    e.svc.poll().await.unwrap();
    assert_eq!(state_of(&e, id).await, TrackedState::ImportPending);
    write(&folder.join("movie.mkv"), 5000);
    assert!(e.svc.process_download(id).await.unwrap());
    assert_eq!(state_of(&e, id).await, TrackedState::Imported);
    // The retry API queues a job.
    let id2 = track(
        &e,
        "nzo2",
        "The.Matrix.1999.720p.HDTV-X",
        Some(movie),
        None,
        vec![],
    )
    .await;
    assert!(e.svc.retry_import(id2).await.unwrap().is_some());
}

#[test]
fn import_mode_defaults() {
    assert_eq!(
        MediaManagementSettings::default().import_mode,
        ImportMode::HardlinkOrCopy
    );
}

/// Old client history in a shared category (e.g. imported by another tool long ago) is not
/// adopted; recent completions and in-progress downloads still are.
#[tokio::test]
async fn monitor_ignores_long_finished_unknown_downloads() {
    let e = env(Protocol::Usenet, false).await;
    let now = reelix_db::now_ms() / 1000;
    let finished = |id: &str, state: DownloadState, ago: Option<i64>| {
        let mut i = item(
            id,
            &format!("Some.Release.{id}.2020.1080p.WEB-DL-GRP"),
            state,
            None,
        );
        i.completed_at = ago.map(|a| now - a);
        i
    };
    e.client.items.lock().unwrap().extend([
        finished("old_done", DownloadState::Completed, Some(3 * 24 * 3600)),
        finished("old_failed", DownloadState::Failed, Some(40 * 24 * 3600)),
        finished("new_done", DownloadState::Completed, Some(600)),
        finished("no_time", DownloadState::Completed, None),
        finished("active", DownloadState::Downloading, None),
    ]);
    e.svc.poll().await.unwrap();
    let mut ids: Vec<String> = e
        .svc
        .queue(None, None)
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|q| q.download_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["active", "new_done", "no_time"]);
    // Still ignored on later polls.
    e.svc.poll().await.unwrap();
    assert_eq!(e.svc.queue(None, None).await.unwrap().total, 3);
}

#[tokio::test]
async fn bulk_remove_reports_each_failure_and_removes_the_rest() {
    let e = env(Protocol::Torrent, false).await;
    e.client.items.lock().unwrap().extend([
        item(
            "h1",
            "First.Thing.2020.1080p.WEB-DL-GRP",
            DownloadState::Downloading,
            None,
        ),
        item(
            "h2",
            "Second.Thing.2020.1080p.WEB-DL-GRP",
            DownloadState::Downloading,
            None,
        ),
    ]);
    e.svc.poll().await.unwrap();
    let ids: Vec<i64> = e
        .svc
        .queue(None, None)
        .await
        .unwrap()
        .items
        .iter()
        .map(|q| q.id)
        .collect();
    assert_eq!(ids.len(), 2);
    let result = e
        .svc
        .remove_many(&QueueBulkRemove {
            ids: vec![ids[0], ids[1], ids[0], 999_999],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.removed, 2);
    assert_eq!(result.failed.len(), 1);
    assert_eq!(result.failed[0].id, 999_999);
    assert_eq!(e.svc.queue(None, None).await.unwrap().total, 0);
    // Removed without touching the client, the rows stay and are not adopted again.
    assert_eq!(e.client.items.lock().unwrap().len(), 2);
    e.svc.poll().await.unwrap();
    assert_eq!(e.svc.queue(None, None).await.unwrap().total, 0);
    let too_many = QueueBulkRemove {
        ids: (0..1001).collect(),
        ..Default::default()
    };
    assert!(e.svc.remove_many(&too_many).await.is_err());
}

/// Copies RAR fixtures from `reelix-import` into a download folder. False when `unrar` is not on
/// PATH (the unpacking tests in `reelix-import` say when that is not allowed).
fn rar_download(folder: &Path, archives: &[&str]) -> bool {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../reelix-import/tests/fixtures/rar");
    std::fs::create_dir_all(folder).unwrap();
    for a in archives {
        std::fs::copy(fixtures.join(a), folder.join(a)).unwrap();
    }
    std::process::Command::new("unrar")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

#[tokio::test]
async fn completed_torrent_with_only_rar_archives_is_unpacked_and_imported() {
    let e = env(Protocol::Torrent, false).await;
    let movie = add_movie(&e).await;
    let title = "The.Matrix.1999.1080p.BluRay.x264-GRP";
    let id = track(&e, "hash1", title, Some(movie), None, vec![]).await;
    let folder = e.downloads.join(title);
    let parts = ["split.part1.rar", "split.part2.rar", "split.part3.rar"];
    if !rar_download(&folder, &parts) {
        return;
    }
    e.client
        .items
        .lock()
        .unwrap()
        .push(item("hash1", title, DownloadState::Completed, Some(&folder)));

    e.svc.poll().await.unwrap();
    assert_eq!(state_of(&e, id).await, TrackedState::Imported);
    let lib = files_under(&e.movies.join("The Matrix (1999)"));
    assert_eq!(lib.len(), 1, "{lib:?}");
    assert_eq!(std::fs::metadata(&lib[0]).unwrap().len(), 6000);
    // The archives stay for seeding; the unpacked copy is gone once it is in the library.
    let left: Vec<String> = files_under(&folder)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, parts);
    assert_eq!(history_events(&e).await, vec!["imported"]);
}

#[tokio::test]
async fn password_protected_rar_waits_with_a_clear_message() {
    let e = env(Protocol::Torrent, false).await;
    let movie = add_movie(&e).await;
    let title = "The.Matrix.1999.1080p.BluRay.x264-GRP";
    let id = track(&e, "hash1", title, Some(movie), None, vec![]).await;
    let folder = e.downloads.join(title);
    if !rar_download(&folder, &["password.rar"]) {
        return;
    }
    e.client
        .items
        .lock()
        .unwrap()
        .push(item("hash1", title, DownloadState::Completed, Some(&folder)));

    e.svc.poll().await.unwrap();
    let t = tracked::get(&e.db, id).await.unwrap().unwrap();
    assert_eq!(t.state, TrackedState::ImportPending);
    assert_eq!(
        t.status_messages,
        ["password.rar: the archive is password protected"]
    );
    // The next poll leaves it waiting instead of trying again.
    e.svc.poll().await.unwrap();
    assert_eq!(state_of(&e, id).await, TrackedState::ImportPending);
    assert_eq!(files_under(&folder), [folder.join("password.rar")]);
}
