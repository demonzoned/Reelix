//! End-to-end migrations from mocked Radarr, Sonarr, Prowlarr and Jellyseerr servers into real
//! Reelix services (with fake TMDB metadata).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use reelix_api_types::migrate::{
    MigrateAction, MigrateCategory, MigrateConnection, MigrateItem, MigrateRunDto, MigrateRunRequest,
    MigrateState, PathRewrite, SourceKind,
};
use reelix_api_types::providers::{IndexerSettingsDto, ProviderKind};
use reelix_api_types::requests::RequestStatus;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_library::{LibraryService, MetadataSource};
use reelix_metadata::*;
use reelix_migrate::{MigrateService, Targets};
use reelix_providers::{ProviderOptions, ProviderService};
use reelix_requests::repo::{RequestFilter, RequestRepo, UserAdminRepo};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------- fake TMDB

fn movie(id: u32, title: &str, year: i32) -> MovieMetadata {
    MovieMetadata {
        tmdb_id: id,
        imdb_id: Some(format!("tt{id:07}")),
        title: title.into(),
        original_title: title.into(),
        original_language: Some("en".into()),
        sort_title: title.to_lowercase(),
        overview: None,
        tagline: None,
        year: Some(year),
        release_date: None,
        release_dates: ReleaseDates::default(),
        certification: None,
        status: MovieStatus::Released,
        runtime_minutes: Some(120),
        genres: vec![],
        keywords: vec![],
        studios: vec![],
        collection: None,
        alternative_titles: vec![],
        external_ids: ExternalIds::default(),
        credits: Credits::default(),
        images: Images::default(),
        videos: vec![],
        ratings: Ratings::default(),
        adult: false,
        homepage: None,
        recommendations: vec![],
        similar: vec![],
    }
}

fn episode(season: u32, n: u32) -> EpisodeMetadata {
    EpisodeMetadata {
        season_number: season,
        episode_number: n,
        absolute_number: None,
        title: Some(format!("Episode {n}")),
        overview: None,
        air_date: chrono::NaiveDate::from_ymd_opt(2020 + season as i32, 1, n),
        runtime_minutes: Some(45),
        still: None,
        tmdb_id: None,
        tvdb_id: None,
        finale: None,
        vote_average: None,
    }
}

fn season(n: u32) -> SeasonMetadata {
    let eps = vec![episode(n, 1), episode(n, 2)];
    SeasonMetadata {
        season_number: n,
        name: Some(format!("Season {n}")),
        overview: None,
        air_date: eps[0].air_date,
        poster: None,
        episode_count: 2,
        episodes: eps,
        tmdb_id: None,
        tvdb_id: None,
    }
}

fn show(id: u32, tvdb: u32, title: &str) -> SeriesMetadata {
    SeriesMetadata {
        tmdb_id: Some(id),
        tvdb_id: Some(tvdb),
        imdb_id: None,
        title: title.into(),
        original_title: None,
        sort_title: title.to_lowercase(),
        overview: None,
        year: Some(2020),
        first_air_date: chrono::NaiveDate::from_ymd_opt(2020, 1, 1),
        last_air_date: None,
        next_air_date: None,
        status: SeriesStatus::Continuing,
        show_type: None,
        network: None,
        networks: vec![],
        genres: vec![],
        keywords: vec![],
        runtime_minutes: Some(45),
        certification: None,
        original_language: Some("en".into()),
        origin_country: vec![],
        alternative_titles: vec![],
        external_ids: ExternalIds::default(),
        credits: Credits::default(),
        images: Images::default(),
        videos: vec![],
        ratings: Ratings::default(),
        seasons: vec![season(1), season(2)],
        recommendations: vec![],
        similar: vec![],
    }
}

#[derive(Default)]
struct FakeMeta {
    movies: Mutex<HashMap<u32, MovieMetadata>>,
    series: Mutex<HashMap<u32, SeriesMetadata>>,
}

#[async_trait]
impl MetadataSource for FakeMeta {
    async fn movie(&self, id: u32) -> Result<MovieMetadata, MetaError> {
        self.movies
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(MetaError::NotFound)
    }
    async fn series(&self, id: SeriesExternalId) -> Result<SeriesMetadata, MetaError> {
        let map = self.series.lock().unwrap();
        match id {
            SeriesExternalId::Tmdb(t) => map.get(&t).cloned().ok_or(MetaError::NotFound),
            SeriesExternalId::Tvdb(t) => map
                .values()
                .find(|s| s.tvdb_id == Some(t))
                .cloned()
                .ok_or(MetaError::NotFound),
            SeriesExternalId::Imdb(_) => Err(MetaError::NotFound),
        }
    }
    async fn collection(&self, _id: u32) -> Result<CollectionMetadata, MetaError> {
        Err(MetaError::NotFound)
    }
    async fn search(
        &self,
        _q: &str,
        _k: reelix_core::MediaKind,
        _y: Option<i32>,
    ) -> Result<Vec<SearchHit>, MetaError> {
        Ok(vec![])
    }
    async fn find(
        &self,
        _s: ExternalSource,
        _id: &str,
        _k: reelix_core::MediaKind,
    ) -> Result<Vec<SearchHit>, MetaError> {
        Ok(vec![])
    }
}

// ---------------------------------------------------------------- fixture

struct Env {
    svc: Arc<MigrateService>,
    db: Db,
    providers: Arc<ProviderService>,
    library: Arc<LibraryService>,
    movies_dir: tempfile::TempDir,
    tv_dir: tempfile::TempDir,
    _data: tempfile::TempDir,
}

async fn env() -> Env {
    let data = tempfile::tempdir().unwrap();
    let db = Db::in_memory().await.unwrap();
    let events = EventBus::default();
    let providers = ProviderService::new(
        db.clone(),
        SecretBox::from_key([9u8; 32]),
        events.clone(),
        reqwest::Client::new(),
        ProviderOptions::new(data.path()),
    )
    .await
    .unwrap();
    providers
        .set_indexer_settings(IndexerSettingsDto {
            allow_private_addresses: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let meta = Arc::new(FakeMeta::default());
    meta.movies
        .lock()
        .unwrap()
        .insert(603, movie(603, "The Matrix", 1999));
    meta.movies
        .lock()
        .unwrap()
        .insert(604, movie(604, "The Matrix Reloaded", 2003));
    meta.series
        .lock()
        .unwrap()
        .insert(1399, show(1399, 2399, "The Show"));
    let library = Arc::new(LibraryService::new(db.clone(), events.clone(), meta.clone()));
    let mut mm = library.media_management().await.unwrap();
    mm.analyze_video_files = false;
    library.set_media_management(mm).await.unwrap();
    let targets = Targets {
        db: db.clone(),
        providers: providers.clone(),
        library: Some(library.clone()),
        books: None,
        music: None,
        metadata: meta,
        metadata_ready: Arc::new(|| true),
    };
    Env {
        svc: Arc::new(MigrateService::new(targets, reqwest::Client::new())),
        db,
        providers,
        library,
        movies_dir: tempfile::tempdir().unwrap(),
        tv_dir: tempfile::tempdir().unwrap(),
        _data: data,
    }
}

async fn mock(server: &MockServer, prefix: &str, route: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(format!("{prefix}{route}")))
        .and(header("x-api-key", "srckey"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

fn profile() -> Value {
    json!([{
        "id": 4, "name": "My HD", "upgradeAllowed": true, "cutoff": 7,
        "items": [
            {"quality": {"id": 1, "name": "SDTV"}, "items": [], "allowed": false},
            {"quality": {"id": 3, "name": "WEBDL-1080p"}, "items": [], "allowed": true},
            {"quality": {"id": 7, "name": "Bluray-1080p"}, "items": [], "allowed": true},
            {"quality": {"id": 30, "name": "Bluray-1080p Remux"}, "items": [], "allowed": true}
        ]
    }])
}

fn qbit(category_field: &str, category: &str) -> Value {
    json!([{
        "id": 2, "name": "qBit", "implementation": "QBittorrent", "protocol": "torrent", "enable": true, "priority": 1,
        "tags": [],
        "fields": [
            {"name": "host", "value": "127.0.0.1"}, {"name": "port", "value": 18080},
            {"name": "useSsl", "value": false}, {"name": "username", "value": "admin"},
            {"name": "password", "value": "********"}, {"name": category_field, "value": category},
            {"name": "initialState", "value": 0}
        ]
    }])
}

async fn radarr(env: &Env) -> MockServer {
    let s = MockServer::start().await;
    let v = "/api/v3";
    mock(
        &s,
        v,
        "/system/status",
        json!({"appName": "Radarr", "version": "5.2.6.8376"}),
    )
    .await;
    mock(&s, v, "/tag", json!([{"id": 1, "label": "kids"}])).await;
    mock(
        &s,
        v,
        "/rootfolder",
        json!([{"id": 1, "path": "/old/movies/"}, {"id": 2, "path": "/gone"}]),
    )
    .await;
    mock(&s, v, "/qualityprofile", profile()).await;
    mock(&s, v, "/indexer", json!([
        {"id": 5, "name": "Tracker", "implementation": "Torznab", "protocol": "torrent", "enableRss": true,
         "enableAutomaticSearch": true, "enableInteractiveSearch": true, "priority": 10, "tags": [],
         "fields": [{"name": "baseUrl", "value": "https://tracker.example/torznab/"}, {"name": "apiPath", "value": "/api"},
                    {"name": "apiKey", "value": "********"}, {"name": "categories", "value": [2000, 2040]},
                    {"name": "minimumSeeders", "value": 3}]},
        {"id": 6, "name": "Via Prowlarr", "implementation": "Torznab", "protocol": "torrent", "enableRss": true,
         "fields": [{"name": "baseUrl", "value": "http://prowlarr:9696/12/"}, {"name": "apiKey", "value": "x"}]},
        {"id": 7, "name": "Odd", "implementation": "FileList", "protocol": "torrent", "fields": []}
    ])).await;
    mock(&s, v, "/downloadclient", qbit("movieCategory", "radarr")).await;
    mock(
        &s,
        v,
        "/remotepathmapping",
        json!([{"host": "127.0.0.1", "remotePath": "/downloads/", "localPath": "/old/movies/dl/"}]),
    )
    .await;
    mock(&s, v, "/movie", json!([
        {"id": 1, "title": "The Matrix", "year": 1999, "tmdbId": 603, "path": "/old/movies/The Matrix (1999)",
         "rootFolderPath": "/old/movies/", "monitored": true, "minimumAvailability": "released",
         "qualityProfileId": 4, "tags": [1], "hasFile": false},
        {"id": 2, "title": "Unknown", "year": 2001, "tmdbId": 999999, "path": "/old/movies/Unknown (2001)",
         "rootFolderPath": "/old/movies/", "monitored": false, "qualityProfileId": 4}
    ])).await;
    let _ = env;
    s
}

async fn sonarr() -> MockServer {
    let s = MockServer::start().await;
    let v = "/api/v3";
    mock(
        &s,
        v,
        "/system/status",
        json!({"appName": "Sonarr", "version": "4.0.1"}),
    )
    .await;
    mock(&s, v, "/tag", json!([{"id": 3, "label": "Kids"}])).await;
    mock(&s, v, "/rootfolder", json!([{"id": 1, "path": "/old/tv"}])).await;
    mock(&s, v, "/qualityprofile", profile()).await;
    mock(&s, v, "/indexer", json!([])).await;
    mock(&s, v, "/downloadclient", qbit("tvCategory", "tv-sonarr")).await;
    mock(&s, v, "/remotepathmapping", json!([])).await;
    mock(
        &s,
        v,
        "/series",
        json!([
            {"id": 1, "title": "The Show", "tvdbId": 2399, "tmdbId": 1399, "path": "/old/tv/The Show",
             "rootFolderPath": "/old/tv", "monitored": true, "seasonFolder": true, "seriesType": "standard",
             "qualityProfileId": 4, "tags": [3],
             "seasons": [{"seasonNumber": 1, "monitored": true}, {"seasonNumber": 2, "monitored": false}]}
        ]),
    )
    .await;
    s
}

async fn wait(svc: &MigrateService) -> MigrateRunDto {
    for _ in 0..200 {
        let s = svc.status().unwrap();
        if s.state != MigrateState::Running {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("migration didn't finish");
}

fn find<'a>(items: &'a [MigrateItem], key: &str) -> &'a MigrateItem {
    items
        .iter()
        .find(|i| i.key == key)
        .unwrap_or_else(|| panic!("no item {key} in {items:#?}"))
}

fn connection(env: &Env, kind: SourceKind, server: &MockServer) -> MigrateConnection {
    MigrateConnection {
        source: kind,
        url: server.uri(),
        api_key: "srckey".into(),
        path_rewrites: vec![
            PathRewrite {
                from: "/old/movies".into(),
                to: env.movies_dir.path().to_string_lossy().into_owned(),
            },
            PathRewrite {
                from: "/old/tv".into(),
                to: env.tv_dir.path().to_string_lossy().into_owned(),
            },
        ],
        secrets: Default::default(),
    }
}

#[tokio::test]
async fn radarr_preview_run_and_rerun() {
    let env = env().await;
    let server = radarr(&env).await;
    let mut conn = connection(&env, SourceKind::Radarr, &server);

    // Preview: nothing changes, and every decision is explained.
    let plan = env.svc.preview(&conn).await.unwrap();
    assert_eq!(plan.version, "5.2.6.8376");
    let items = &plan.items;
    assert_eq!(find(items, "tag:1").action, MigrateAction::Create);
    assert_eq!(find(items, "root_folder:1").action, MigrateAction::Create);
    let gone = find(items, "root_folder:2");
    assert_eq!(gone.action, MigrateAction::Skip);
    assert!(gone.detail.as_ref().unwrap().contains("doesn't exist"));
    assert_eq!(find(items, "quality_profile:4").action, MigrateAction::Create);
    let tracker = find(items, "indexer:5");
    assert_eq!(tracker.action, MigrateAction::Skip);
    assert_eq!(tracker.missing_secrets, vec!["apiKey"]);
    assert!(
        find(items, "indexer:6")
            .detail
            .as_ref()
            .unwrap()
            .contains("Prowlarr")
    );
    assert_eq!(find(items, "indexer:7").action, MigrateAction::Skip);
    assert_eq!(find(items, "download_client:2").missing_secrets, vec!["password"]);
    assert_eq!(find(items, "movie:1").action, MigrateAction::Create);
    assert!(env.providers.tags().is_empty(), "a preview changes nothing");

    // Run with the hidden secrets filled in.
    conn.secrets
        .insert("indexer:5:apiKey".into(), "trackerkey".into());
    conn.secrets
        .insert("download_client:2:password".into(), "pw".into());
    env.svc
        .start(MigrateRunRequest {
            connection: conn.clone(),
            categories: vec![],
            search_missing: false,
        })
        .unwrap();
    let run = wait(&env.svc).await;
    assert_eq!(run.state, MigrateState::Finished, "{run:#?}");
    assert_eq!(run.done, run.total);
    let items = &run.items;
    for key in [
        "tag:1",
        "root_folder:1",
        "quality_profile:4",
        "indexer:5",
        "download_client:2",
        "movie:1",
    ] {
        assert_eq!(
            find(items, key).action,
            MigrateAction::Create,
            "{key}: {:?}",
            find(items, key)
        );
    }
    assert_eq!(
        find(items, "movie:2").action,
        MigrateAction::Failed,
        "unknown TMDB id"
    );

    // What landed in Reelix.
    let tag = env
        .providers
        .tags()
        .into_iter()
        .find(|t| t.label == "kids")
        .unwrap();
    let movie = env.library.movie_by_tmdb(603).await.unwrap().unwrap();
    assert!(movie.monitored);
    assert_eq!(movie.tags, vec![tag.id]);
    assert!(movie.path.ends_with("The Matrix (1999)"), "{}", movie.path);
    let profile = env
        .db
        .quality_profiles()
        .get(movie.quality_profile_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(profile.name, "My HD");
    assert!(profile.upgrade_allowed);
    let indexer = env
        .providers
        .list(ProviderKind::Indexer)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.name == "Tracker")
        .unwrap();
    assert_eq!(indexer.implementation, "torznab");
    assert_eq!(indexer.settings["api_key"], "trackerkey");
    assert_eq!(indexer.settings["minimum_seeders"], 3);
    let client = env
        .providers
        .list(ProviderKind::DownloadClient)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(client.implementation, "qbittorrent");
    assert_eq!(client.settings["movie_category"], "radarr");
    assert_eq!(client.settings["password"], "pw");
    assert_eq!(env.providers.remote_path_mappings().len(), 1);

    // Running again reuses everything.
    env.svc
        .start(MigrateRunRequest {
            connection: conn,
            categories: vec![],
            search_missing: false,
        })
        .unwrap();
    let again = wait(&env.svc).await;
    for key in [
        "tag:1",
        "root_folder:1",
        "quality_profile:4",
        "indexer:5",
        "download_client:2",
        "movie:1",
    ] {
        assert_eq!(find(&again.items, key).action, MigrateAction::Existing, "{key}");
    }
    assert_eq!(env.providers.list(ProviderKind::Indexer).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_profile_name_used_by_another_kind_gets_the_apps_name() {
    // Profile names are unique across kinds: a music profile called "My HD" already exists, so
    // Radarr's video profile of that name is created as "My HD (Radarr)" instead of failing.
    let env = env().await;
    let repo = env.db.quality_profiles();
    let music = repo
        .list_all()
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.family == reelix_parser::QualityFamily::Music)
        .expect("a seeded music profile");
    let mut renamed = music.clone();
    renamed.name = "My HD".into();
    repo.update(music.id, &renamed).await.unwrap();

    let server = radarr(&env).await;
    let conn = connection(&env, SourceKind::Radarr, &server);
    env.svc
        .start(MigrateRunRequest {
            connection: conn,
            categories: vec![MigrateCategory::QualityProfile],
            search_missing: false,
        })
        .unwrap();
    let run = wait(&env.svc).await;
    let item = find(&run.items, "quality_profile:4");
    assert_eq!(item.action, MigrateAction::Create, "{item:?}");
    assert!(
        item.detail
            .as_deref()
            .unwrap_or_default()
            .contains("My HD (Radarr)")
    );
    let names: Vec<String> = repo
        .list_all()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert!(names.iter().any(|n| n == "My HD (Radarr)"), "{names:?}");
}

#[tokio::test]
async fn a_renamed_profile_avoids_a_second_clash_and_is_found_again() {
    // "My HD" and "My HD (Radarr)" are both taken by profiles of other kinds, so Radarr's profile
    // becomes "My HD (Radarr) 2"; a second run finds it under that name instead of making another.
    let env = env().await;
    let repo = env.db.quality_profiles();
    let others: Vec<_> = repo
        .list_all()
        .await
        .unwrap()
        .into_iter()
        .filter(|p| p.family != reelix_parser::QualityFamily::Video)
        .collect();
    assert!(others.len() >= 2, "two seeded profiles of other kinds");
    for (p, name) in others.iter().zip(["My HD", "My HD (Radarr)"]) {
        let mut renamed = p.clone();
        renamed.name = name.into();
        repo.update(p.id, &renamed).await.unwrap();
    }

    let server = radarr(&env).await;
    for round in 0..2 {
        let conn = connection(&env, SourceKind::Radarr, &server);
        env.svc
            .start(MigrateRunRequest {
                connection: conn,
                categories: vec![MigrateCategory::QualityProfile],
                search_missing: false,
            })
            .unwrap();
        let run = wait(&env.svc).await;
        let item = find(&run.items, "quality_profile:4");
        let want = if round == 0 {
            MigrateAction::Create
        } else {
            MigrateAction::Existing
        };
        assert_eq!(item.action, want, "round {round}: {item:?}");
    }
    let names: Vec<String> = repo
        .list_all()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(
        names.iter().filter(|n| n.starts_with("My HD (Radarr)")).count(),
        2,
        "{names:?}"
    );
    assert!(names.iter().any(|n| n == "My HD (Radarr) 2"), "{names:?}");
}

#[tokio::test]
async fn sonarr_after_radarr_shares_the_download_client() {
    let env = env().await;
    let r = radarr(&env).await;
    let mut conn = connection(&env, SourceKind::Radarr, &r);
    conn.secrets
        .insert("download_client:2:password".into(), "pw".into());
    env.svc
        .start(MigrateRunRequest {
            connection: conn,
            categories: vec![MigrateCategory::DownloadClient],
            search_missing: false,
        })
        .unwrap();
    let run = wait(&env.svc).await;
    assert!(
        run.items
            .iter()
            .all(|i| i.category == MigrateCategory::DownloadClient),
        "{run:#?}"
    );
    assert!(
        env.providers.tags().is_empty(),
        "only the chosen categories are created"
    );

    let s = sonarr().await;
    let conn = connection(&env, SourceKind::Sonarr, &s);
    env.svc
        .start(MigrateRunRequest {
            connection: conn,
            categories: vec![],
            search_missing: false,
        })
        .unwrap();
    let run = wait(&env.svc).await;
    assert_eq!(run.state, MigrateState::Finished, "{run:#?}");
    let dc = find(&run.items, "download_client:2");
    assert_eq!(dc.action, MigrateAction::Existing);
    assert!(dc.detail.as_ref().unwrap().contains("tv category"), "{dc:?}");
    let client = env.providers.list(ProviderKind::DownloadClient).await.unwrap();
    assert_eq!(client.len(), 1);
    assert_eq!(client[0].settings["movie_category"], "radarr");
    assert_eq!(client[0].settings["tv_category"], "tv-sonarr");

    assert_eq!(
        find(&run.items, "series:1").action,
        MigrateAction::Create,
        "{run:#?}"
    );
    let show = env.library.series_by_tmdb(1399).await.unwrap().unwrap();
    let monitored: Vec<(u32, bool)> = show
        .seasons
        .iter()
        .map(|s| (s.season_number, s.monitored))
        .collect();
    assert!(
        monitored.contains(&(1, true)) && monitored.contains(&(2, false)),
        "{monitored:?}"
    );
}

#[tokio::test]
async fn jellyseerr_users_and_requests() {
    let env = env().await;
    // The requested movie is already in the library.
    let root = env
        .library
        .add_root_folder(
            env.movies_dir.path().to_str().unwrap(),
            reelix_core::MediaKind::Movie,
        )
        .await
        .unwrap();
    let profile = env.db.quality_profiles().list().await.unwrap().remove(0);
    env.library
        .add_movie(reelix_api_types::library::AddMovieRequest {
            tmdb_id: 603,
            quality_profile_id: profile.id,
            root_folder_id: root.id,
            monitor: Default::default(),
            minimum_availability: Default::default(),
            tags: vec![],
            search_on_add: false,
            path: None,
            added_by_request_id: None,
        })
        .await
        .unwrap();

    let s = MockServer::start().await;
    let v = "/api/v1";
    let alice = json!({"id": 1, "email": "alice@example.invalid", "jellyfinUsername": "alice", "userType": 3,
        "permissions": 32 | 1024, "jellyfinUserId": "jf-alice", "movieQuotaLimit": 5, "movieQuotaDays": 7});
    let bob = json!({"id": 2, "username": "bob", "userType": 2, "permissions": 2});
    mock(&s, v, "/status", json!({"version": "2.1.0"})).await;
    mock(
        &s,
        v,
        "/user",
        json!({"pageInfo": {"pages": 1}, "results": [alice.clone(), bob]}),
    )
    .await;
    mock(&s, v, "/request", json!({"pageInfo": {"pages": 1}, "results": [
        {"id": 10, "status": 2, "type": "movie", "is4k": false, "createdAt": "2024-05-01T10:00:00.000Z",
         "media": {"tmdbId": 603, "mediaType": "movie", "status": 5}, "requestedBy": alice.clone(), "seasons": []},
        {"id": 11, "status": 1, "type": "tv", "is4k": false, "createdAt": "2024-06-01T10:00:00.000Z",
         "media": {"tmdbId": 1399, "tvdbId": 2399, "mediaType": "tv", "status": 2}, "requestedBy": alice,
         "seasons": [{"seasonNumber": 1}]},
        {"id": 12, "status": 1, "type": "movie", "media": {"tmdbId": 604, "mediaType": "movie"},
         "requestedBy": {"id": 99, "username": "ghost"}}
    ]})).await;

    let conn = MigrateConnection {
        source: SourceKind::Jellyseerr,
        url: s.uri(),
        api_key: "srckey".into(),
        path_rewrites: vec![],
        secrets: Default::default(),
    };
    let plan = env.svc.preview(&conn).await.unwrap();
    assert_eq!(
        find(&plan.items, "request:10").action,
        MigrateAction::Create,
        "{plan:#?}"
    );
    assert!(
        find(&plan.items, "user:2")
            .detail
            .as_ref()
            .unwrap()
            .contains("password")
    );
    assert_eq!(find(&plan.items, "request:12").action, MigrateAction::Skip);

    env.svc
        .start(MigrateRunRequest {
            connection: conn.clone(),
            categories: vec![],
            search_missing: false,
        })
        .unwrap();
    let run = wait(&env.svc).await;
    assert_eq!(run.state, MigrateState::Finished, "{run:#?}");
    let users = UserAdminRepo(&env.db);
    let a = users
        .by_external("jellyfin", "jf-alice")
        .await
        .unwrap()
        .expect("alice linked");
    assert_eq!(a.username, "alice");
    let bob = users
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|u| u.username == "bob")
        .unwrap();
    assert!(bob.permissions.contains(reelix_core::Permissions::ADMIN));

    let requests = RequestRepo(&env.db)
        .find(&RequestFilter {
            user_id: Some(a.id),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(requests.len(), 2, "{requests:#?}");
    let m = requests.iter().find(|r| r.tmdb_id == 603).unwrap();
    assert_eq!(m.status, RequestStatus::Available);
    assert_eq!(m.title, "The Matrix");
    assert!(m.movie_id.is_some(), "linked to the library movie");
    assert_eq!(
        m.created_at,
        chrono::DateTime::parse_from_rfc3339("2024-05-01T10:00:00Z")
            .unwrap()
            .timestamp_millis()
    );
    let tv = requests.iter().find(|r| r.tmdb_id == 1399).unwrap();
    assert_eq!(tv.status, RequestStatus::Pending);
    assert_eq!(tv.seasons, vec![1]);

    // Again: nothing is duplicated.
    env.svc
        .start(MigrateRunRequest {
            connection: conn,
            categories: vec![],
            search_missing: false,
        })
        .unwrap();
    let again = wait(&env.svc).await;
    assert_eq!(find(&again.items, "user:1").action, MigrateAction::Existing);
    assert_eq!(find(&again.items, "request:10").action, MigrateAction::Existing);
}

#[tokio::test]
async fn rejects_the_wrong_app_and_bad_keys() {
    let env = env().await;
    let s = MockServer::start().await;
    mock(
        &s,
        "/api/v3",
        "/system/status",
        json!({"appName": "Sonarr", "version": "4"}),
    )
    .await;
    let conn = MigrateConnection {
        source: SourceKind::Radarr,
        url: s.uri(),
        api_key: "srckey".into(),
        path_rewrites: vec![],
        secrets: Default::default(),
    };
    let e = env.svc.test(&conn).await.unwrap_err().to_string();
    assert!(e.contains("Sonarr"), "{e}");
    let wrong_key = MigrateConnection {
        api_key: "nope".into(),
        ..conn
    };
    let e = env.svc.test(&wrong_key).await.unwrap_err().to_string();
    assert!(e.contains("HTTP 404") || e.contains("rejected"), "{e}");
}
