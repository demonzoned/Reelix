//! Library service flows against a fake metadata source and temp directories.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::NaiveDate;
use reelix_api_types::library::*;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_library::{AvailabilityStatus, LibraryError, LibraryService, MetadataSource};
use reelix_metadata::*;

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn hit(kind: reelix_core::MediaKind, id: u32, title: &str, year: i32) -> SearchHit {
    SearchHit {
        kind,
        tmdb_id: Some(id),
        tvdb_id: None,
        imdb_id: None,
        title: title.into(),
        original_title: None,
        year: Some(year),
        release_date: None,
        overview: None,
        poster: Some(format!("https://image.tmdb.org/t/p/w500/{id}.jpg")),
        backdrop: None,
        genre_ids: vec![],
        original_language: Some("en".into()),
        vote_average: 7.5,
        vote_count: 100,
        popularity: 10.0,
        adult: false,
    }
}

fn movie(id: u32, title: &str, year: i32, digital: Option<&str>, collection: Option<u32>) -> MovieMetadata {
    MovieMetadata {
        tmdb_id: id,
        imdb_id: Some(format!("tt{id:07}")),
        title: title.into(),
        original_title: title.into(),
        original_language: Some("en".into()),
        sort_title: sort_title(title),
        overview: Some("An overview".into()),
        tagline: None,
        year: Some(year),
        release_date: Some(d(&format!("{year}-03-01"))),
        release_dates: ReleaseDates {
            cinema: Some(d(&format!("{year}-03-01"))),
            digital: digital.map(d),
            physical: None,
            all: vec![],
        },
        certification: Some("R".into()),
        status: reelix_metadata::MovieStatus::Released,
        runtime_minutes: Some(120),
        genres: vec![Genre {
            id: 28,
            name: "Action".into(),
        }],
        keywords: vec![],
        studios: vec![],
        collection: collection.map(|c| CollectionRef {
            tmdb_id: c,
            name: "The Matrix Collection".into(),
            poster: None,
            backdrop: None,
        }),
        alternative_titles: vec![AlternativeTitle {
            title: format!("{title} Alt"),
            country: Some("US".into()),
            kind: None,
        }],
        external_ids: ExternalIds::default(),
        credits: Credits {
            cast: vec![CastMember {
                tmdb_id: 1,
                name: "Actor One".into(),
                character: Some("Hero".into()),
                order: 0,
                profile_url: None,
            }],
            crew: vec![],
        },
        images: Images::default(),
        videos: vec![],
        ratings: Ratings::default(),
        adult: false,
        homepage: None,
        recommendations: vec![],
        similar: vec![],
    }
}

fn episode(season: u32, n: u32, air: &str) -> EpisodeMetadata {
    EpisodeMetadata {
        season_number: season,
        episode_number: n,
        absolute_number: None,
        title: Some(format!("Episode {n}")),
        overview: None,
        air_date: Some(d(air)),
        runtime_minutes: Some(45),
        still: None,
        tmdb_id: None,
        tvdb_id: None,
        finale: None,
        vote_average: None,
    }
}

fn season(n: u32, eps: Vec<EpisodeMetadata>) -> SeasonMetadata {
    SeasonMetadata {
        season_number: n,
        name: Some(format!("Season {n}")),
        overview: None,
        air_date: eps.first().and_then(|e| e.air_date),
        poster: None,
        episode_count: eps.len() as u32,
        episodes: eps,
        tmdb_id: None,
        tvdb_id: None,
    }
}

fn show(id: u32, seasons: Vec<SeasonMetadata>) -> SeriesMetadata {
    SeriesMetadata {
        tmdb_id: Some(id),
        tvdb_id: Some(id + 1000),
        imdb_id: None,
        title: "The Show".into(),
        original_title: None,
        sort_title: "show".into(),
        overview: None,
        year: Some(2020),
        first_air_date: Some(d("2020-01-01")),
        last_air_date: None,
        next_air_date: None,
        status: reelix_metadata::SeriesStatus::Continuing,
        show_type: None,
        network: Some("HBO".into()),
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
        seasons,
        recommendations: vec![],
        similar: vec![],
    }
}

#[derive(Default)]
struct FakeMeta {
    movies: Mutex<HashMap<u32, MovieMetadata>>,
    series: Mutex<HashMap<u32, SeriesMetadata>>,
    collections: Mutex<HashMap<u32, CollectionMetadata>>,
    /// Answer a TMDB id it does not have with `Unsupported`, as open data does, not `NotFound`.
    open_data: std::sync::atomic::AtomicBool,
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
            SeriesExternalId::Tmdb(t) => {
                map.get(&t)
                    .cloned()
                    .ok_or(if self.open_data.load(std::sync::atomic::Ordering::Relaxed) {
                        MetaError::Unsupported("not on Wikidata")
                    } else {
                        MetaError::NotFound
                    })
            }
            SeriesExternalId::Tvdb(t) => map
                .values()
                .find(|s| s.tvdb_id == Some(t))
                .cloned()
                .ok_or(MetaError::NotFound),
            SeriesExternalId::Imdb(_) => Err(MetaError::NotFound),
        }
    }
    async fn collection(&self, id: u32) -> Result<CollectionMetadata, MetaError> {
        self.collections
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(MetaError::NotFound)
    }
    async fn search(
        &self,
        q: &str,
        kind: reelix_core::MediaKind,
        _year: Option<i32>,
    ) -> Result<Vec<SearchHit>, MetaError> {
        let q = q.to_lowercase();
        Ok(match kind {
            reelix_core::MediaKind::Movie => self
                .movies
                .lock()
                .unwrap()
                .values()
                .filter(|m| m.title.to_lowercase().contains(&q))
                .map(|m| hit(kind, m.tmdb_id, &m.title, m.year.unwrap_or(0)))
                .collect(),
            reelix_core::MediaKind::Series => self
                .series
                .lock()
                .unwrap()
                .values()
                .filter(|s| s.title.to_lowercase().contains(&q))
                .map(|s| hit(kind, s.tmdb_id.unwrap(), &s.title, 2020))
                .collect(),
        })
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

struct Env {
    svc: LibraryService,
    meta: Arc<FakeMeta>,
    movies_root: tempfile::TempDir,
    tv_root: tempfile::TempDir,
    movie_root_id: i64,
    tv_root_id: i64,
    db: Db,
}

async fn env() -> Env {
    let db = Db::in_memory().await.unwrap();
    let meta = Arc::new(FakeMeta::default());
    meta.movies.lock().unwrap().insert(
        603,
        movie(603, "The Matrix", 1999, Some("1999-09-21"), Some(2344)),
    );
    meta.movies.lock().unwrap().insert(
        604,
        movie(604, "The Matrix Reloaded", 2003, Some("2003-10-14"), Some(2344)),
    );
    meta.movies
        .lock()
        .unwrap()
        .insert(900, movie(900, "Future Film", 2030, None, None));
    meta.collections.lock().unwrap().insert(
        2344,
        CollectionMetadata {
            tmdb_id: 2344,
            name: "The Matrix Collection".into(),
            overview: None,
            images: Images::default(),
            parts: vec![
                hit(reelix_core::MediaKind::Movie, 603, "The Matrix", 1999),
                hit(reelix_core::MediaKind::Movie, 604, "The Matrix Reloaded", 2003),
            ],
        },
    );
    meta.series.lock().unwrap().insert(
        1399,
        show(
            1399,
            vec![
                season(0, vec![episode(0, 1, "2019-12-01")]),
                season(1, vec![episode(1, 1, "2020-01-01"), episode(1, 2, "2020-01-08")]),
                season(2, vec![episode(2, 1, "2024-07-01"), episode(2, 2, "2024-07-08")]),
            ],
        ),
    );
    let svc = LibraryService::new(db.clone(), EventBus::default(), meta.clone());
    svc.set_today(d("2024-07-03"));
    let mut mm = svc.media_management().await.unwrap();
    mm.analyze_video_files = false;
    svc.set_media_management(mm).await.unwrap();
    let movies_root = tempfile::tempdir().unwrap();
    let tv_root = tempfile::tempdir().unwrap();
    let movie_root_id = svc
        .add_root_folder(
            movies_root.path().to_str().unwrap(),
            reelix_core::MediaKind::Movie,
        )
        .await
        .unwrap()
        .id;
    let tv_root_id = svc
        .add_root_folder(tv_root.path().to_str().unwrap(), reelix_core::MediaKind::Series)
        .await
        .unwrap()
        .id;
    Env {
        svc,
        meta,
        movies_root,
        tv_root,
        movie_root_id,
        tv_root_id,
        db,
    }
}

fn add_movie_req(e: &Env, tmdb: u32) -> AddMovieRequest {
    AddMovieRequest {
        tmdb_id: tmdb,
        quality_profile_id: 1,
        root_folder_id: e.movie_root_id,
        monitor: MovieMonitor::MovieOnly,
        minimum_availability: MinimumAvailability::Released,
        tags: vec![2, 1],
        search_on_add: false,
        path: None,
        added_by_request_id: None,
    }
}

/// Sparse file of `mb` megabytes (large enough not to count as a sample).
fn write(path: &Path, mb: u64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::File::create(path)
        .unwrap()
        .set_len(mb * 1024 * 1024)
        .unwrap();
}
const MB: u64 = 1024 * 1024;

#[tokio::test]
async fn add_movie_list_edit_and_conflicts() {
    let e = env().await;
    let m = e.svc.add_movie(add_movie_req(&e, 603)).await.unwrap();
    assert_eq!(m.title, "The Matrix");
    assert_eq!(m.status, reelix_api_types::library::MovieStatus::Released);
    assert!(m.is_available);
    assert_eq!(m.tags, vec![1, 2]);
    assert!(m.path.ends_with("The Matrix (1999) {tmdb-603}"), "{}", m.path);
    assert_eq!(m.collection.as_ref().unwrap().tmdb_id, 2344);
    assert_eq!(m.cast.len(), 1);
    assert_eq!(m.alternative_titles, vec!["The Matrix Alt".to_string()]);

    let dup = e.svc.add_movie(add_movie_req(&e, 603)).await;
    assert!(matches!(dup, Err(LibraryError::Conflict(_))));
    let mut bad = add_movie_req(&e, 604);
    bad.root_folder_id = e.tv_root_id;
    assert!(matches!(
        e.svc.add_movie(bad).await,
        Err(LibraryError::Validation(_))
    ));
    let mut bad = add_movie_req(&e, 604);
    bad.quality_profile_id = 999;
    assert!(matches!(
        e.svc.add_movie(bad).await,
        Err(LibraryError::Validation(_))
    ));

    let other = e.svc.add_movie(add_movie_req(&e, 900)).await.unwrap();
    let list = e.svc.list_movies(&MovieListQuery::default()).await.unwrap();
    assert_eq!(list.total, 2);
    // One movie's tags and alternative titles are read for that movie only.
    let one = e.svc.get_movie(m.id).await.unwrap();
    assert_eq!(one.alternative_titles, vec!["The Matrix Alt".to_string()]);
    assert_eq!(one.tags, vec![1, 2]);
    let two = e.svc.get_movie(other.id).await.unwrap();
    assert_eq!(two.alternative_titles, other.alternative_titles);
    assert_ne!(two.alternative_titles, one.alternative_titles);
    let missing = e
        .svc
        .list_movies(&MovieListQuery {
            missing: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(missing.items.len(), 1, "only the released movie is missing");
    let q = e
        .svc
        .list_movies(&MovieListQuery {
            q: Some("matrix alt".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(q.total, 1);

    let edited = e
        .svc
        .update_movie(
            m.id,
            MovieEdit {
                monitored: Some(false),
                tags: Some(vec![3]),
                tag_mode: TagMode::Add,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!edited.monitored);
    assert_eq!(edited.tags, vec![1, 2, 3]);

    let bulk = e
        .svc
        .bulk_update_movies(MovieBulkEdit {
            ids: vec![m.id],
            edit: MovieEdit {
                minimum_availability: Some(MinimumAvailability::InCinemas),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    assert_eq!(bulk[0].minimum_availability, MinimumAvailability::InCinemas);

    // Lookup flags library membership.
    let hits = e.svc.lookup_movies("matrix").await.unwrap();
    let matrix = hits.iter().find(|h| h.tmdb_id == Some(603)).unwrap();
    assert!(matrix.in_library);
    assert_eq!(matrix.library_id, Some(m.id));
    let reloaded = hits.iter().find(|h| h.tmdb_id == Some(604)).unwrap();
    assert!(!reloaded.in_library);
    assert_eq!(
        e.svc.lookup_movies("tmdb:604").await.unwrap()[0].title,
        "The Matrix Reloaded"
    );

    // Availability facts for requests.
    let av = e.svc.movie_availability(&[603, 604]).await.unwrap();
    assert_eq!(av[&603].status, AvailabilityStatus::Unmonitored);
    assert_eq!(av[&604].status, AvailabilityStatus::NotInLibrary);
}

#[tokio::test]
async fn a_series_the_source_cannot_find_by_tmdb_id_is_found_by_tvdb_and_not_deleted() {
    // Open data (no TMDB key) can't always turn a TMDB id into a show; the TVDB id is tried next,
    // the known TMDB id is kept, and a refresh doesn't mark the series deleted.
    let e = env().await;
    e.meta.open_data.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut s = show(7000, vec![season(1, vec![episode(1, 1, "2020-01-01")])]);
    s.tmdb_id = None; // the source doesn't know the TMDB id
    s.tvdb_id = Some(8000);
    e.meta.series.lock().unwrap().insert(0, s);
    let added = e
        .svc
        .add_series(AddSeriesRequest {
            tmdb_id: Some(7000),
            tvdb_id: Some(8000),
            quality_profile_id: 1,
            root_folder_id: e.tv_root_id,
            monitor: SeriesMonitor::All,
            seasons: None,
            series_type: SeriesType::Standard,
            season_folder: true,
            monitor_new_items: NewItemMonitor::All,
            tags: vec![],
            search_on_add: false,
            search_cutoff_unmet: false,
            path: None,
            added_by_request_id: None,
        })
        .await
        .unwrap();
    assert_eq!(added.tmdb_id, Some(7000));
    e.svc.refresh_series(added.id).await.unwrap();
    let after = e.svc.get_series(added.id).await.unwrap();
    assert_ne!(format!("{:?}", after.status).to_lowercase(), "deleted");
}

fn add_series_req(e: &Env, tmdb_id: u32, tvdb_id: u32) -> AddSeriesRequest {
    AddSeriesRequest {
        tmdb_id: Some(tmdb_id),
        tvdb_id: Some(tvdb_id),
        quality_profile_id: 1,
        root_folder_id: e.tv_root_id,
        monitor: SeriesMonitor::All,
        seasons: None,
        series_type: SeriesType::Standard,
        season_folder: true,
        monitor_new_items: NewItemMonitor::All,
        tags: vec![],
        search_on_add: false,
        search_cutoff_unmet: false,
        path: None,
        added_by_request_id: None,
    }
}

#[tokio::test]
async fn a_tmdb_id_paired_with_a_tvdb_id_already_in_the_library_is_refused() {
    // A client sends a new TMDB id with the TVDB id of a show already added: the fallback finds
    // that show by its TVDB id, and adding it again would make a second row in the same folder.
    let e = env().await;
    e.meta.open_data.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut s = show(7100, vec![season(1, vec![episode(1, 1, "2020-01-01")])]);
    s.tmdb_id = None;
    s.tvdb_id = Some(8100);
    e.meta.series.lock().unwrap().insert(0, s);
    e.svc.add_series(add_series_req(&e, 7100, 8100)).await.unwrap();
    let again = e.svc.add_series(add_series_req(&e, 7199, 8100)).await;
    assert!(matches!(again, Err(LibraryError::Conflict(_))), "{again:?}");
    assert_eq!(
        e.svc
            .list_series(&SeriesListQuery::default())
            .await
            .unwrap()
            .total,
        1
    );
}

#[tokio::test]
async fn a_tvdb_id_linked_to_another_tmdb_entry_is_not_used() {
    // The TVDB result names its own TMDB entry, so the request's TMDB id is a different show.
    let e = env().await;
    e.meta.open_data.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut s = show(7200, vec![season(1, vec![episode(1, 1, "2020-01-01")])]);
    s.tvdb_id = Some(8200);
    e.meta.series.lock().unwrap().insert(0, s);
    let r = e.svc.add_series(add_series_req(&e, 7299, 8200)).await;
    assert!(r.is_err(), "{r:?}");
    assert_eq!(
        e.svc
            .list_series(&SeriesListQuery::default())
            .await
            .unwrap()
            .total,
        0
    );
}

#[tokio::test]
async fn a_series_removed_from_tmdb_is_marked_deleted_not_moved_by_its_tvdb_id() {
    // With a TMDB key, NotFound means removed from TMDB: the TVDB id is not tried, so the row is
    // not moved to whatever TMDB entry that TVDB id has now.
    let e = env().await;
    e.meta.series.lock().unwrap().insert(
        7300,
        show(7300, vec![season(1, vec![episode(1, 1, "2020-01-01")])]),
    );
    let added = e.svc.add_series(add_series_req(&e, 7300, 8300)).await.unwrap();
    let moved = {
        let mut map = e.meta.series.lock().unwrap();
        let mut s = map.remove(&7300).unwrap();
        s.tmdb_id = Some(7301);
        s
    };
    e.meta.series.lock().unwrap().insert(7301, moved);
    e.svc.refresh_series(added.id).await.unwrap();
    let after = e.svc.get_series(added.id).await.unwrap();
    assert_eq!(after.tmdb_id, Some(7300));
    assert_eq!(format!("{:?}", after.status).to_lowercase(), "deleted");
}

#[tokio::test]
async fn scan_rename_and_delete_movie_files() {
    let e = env().await;
    let m = e.svc.add_movie(add_movie_req(&e, 603)).await.unwrap();
    let folder = Path::new(&m.path);
    write(&folder.join("The.Matrix.1999.1080p.BluRay.x264-GRP.mkv"), 800);
    write(&folder.join("sample.mkv"), 1);
    let r = e.svc.scan_movie(m.id).await.unwrap();
    assert_eq!(r.added, 1);
    let m2 = e.svc.get_movie(m.id).await.unwrap();
    assert!(m2.has_file);
    let f = m2.movie_file.as_ref().unwrap();
    assert_eq!(f.file.quality_name, "Bluray-1080p");
    assert_eq!(f.file.release_group.as_deref(), Some("GRP"));
    assert_eq!(m2.size_on_disk, 800 * MB);

    // Rescan is idempotent.
    let r = e.svc.scan_movie(m.id).await.unwrap();
    assert_eq!((r.added, r.removed), (0, 0));

    let preview = e
        .svc
        .rename_preview(&RenamePreviewQuery {
            movie_id: Some(m.id),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(preview.len(), 1);
    assert!(
        preview[0].new_path.starts_with("The Matrix (1999)"),
        "{}",
        preview[0].new_path
    );
    assert!(
        preview[0].new_path.ends_with("-GRP.mkv"),
        "{}",
        preview[0].new_path
    );
    let n = e
        .svc
        .execute_rename(&RenameRequest {
            kind: reelix_core::MediaKind::Movie,
            ids: vec![m.id],
            file_ids: None,
        })
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert!(folder.join(&preview[0].new_path).exists());
    let files = e.svc.movie_files(m.id).await.unwrap();
    assert_eq!(files[0].file.relative_path, preview[0].new_path);
    assert!(
        e.svc
            .rename_preview(&RenamePreviewQuery {
                movie_id: Some(m.id),
                ..Default::default()
            })
            .await
            .unwrap()
            .is_empty()
    );

    // Recycle bin on file delete.
    let bin = tempfile::tempdir().unwrap();
    let mut mm = e.svc.media_management().await.unwrap();
    mm.recycle_bin_path = Some(bin.path().to_string_lossy().into_owned());
    e.svc.set_media_management(mm).await.unwrap();
    e.svc.delete_movie_file(files[0].file.id).await.unwrap();
    assert!(!folder.join(&preview[0].new_path).exists());
    assert!(std::fs::read_dir(bin.path()).unwrap().next().is_some());
    assert!(!e.svc.get_movie(m.id).await.unwrap().has_file);

    // Delete movie with files (folder goes to the bin).
    e.svc.delete_movie(m.id, true).await.unwrap();
    assert!(!folder.exists());
    assert!(matches!(
        e.svc.get_movie(m.id).await,
        Err(LibraryError::NotFound(_))
    ));
}

#[tokio::test]
async fn series_add_monitor_scan_refresh_and_calendar() {
    let e = env().await;
    let s = e
        .svc
        .add_series(AddSeriesRequest {
            tmdb_id: Some(1399),
            tvdb_id: None,
            quality_profile_id: 1,
            root_folder_id: e.tv_root_id,
            monitor: SeriesMonitor::FirstSeason,
            seasons: None,
            series_type: SeriesType::Standard,
            season_folder: true,
            monitor_new_items: NewItemMonitor::All,
            tags: vec![],
            search_on_add: false,
            search_cutoff_unmet: false,
            path: None,
            added_by_request_id: None,
        })
        .await
        .unwrap();
    assert_eq!(s.seasons.len(), 3);
    assert_eq!(s.statistics.total_episode_count, 5);
    let mon: Vec<bool> = s.seasons.iter().map(|x| x.monitored).collect();
    assert_eq!(mon, vec![false, true, false]);
    assert_eq!(s.tvdb_id, Some(2399));
    assert!(s.path.ends_with("The Show (2020) {tvdb-2399}"), "{}", s.path);

    // Duplicate by tvdb id.
    let dup = e
        .svc
        .add_series(AddSeriesRequest {
            tmdb_id: None,
            tvdb_id: Some(2399),
            quality_profile_id: 1,
            root_folder_id: e.tv_root_id,
            monitor: SeriesMonitor::All,
            seasons: None,
            series_type: SeriesType::Standard,
            season_folder: true,
            monitor_new_items: NewItemMonitor::All,
            tags: vec![],
            search_on_add: false,
            search_cutoff_unmet: false,
            path: None,
            added_by_request_id: None,
        })
        .await;
    assert!(matches!(dup, Err(LibraryError::Conflict(_))));

    // Files: S01E01 and a double episode S02E01E02 (S02 is unmonitored).
    let root = Path::new(&s.path);
    write(
        &root.join("Season 01/The.Show.S01E01.720p.HDTV.x264-GRP.mkv"),
        400,
    );
    write(
        &root.join("Season 02/The.Show.S02E01E02.1080p.WEB-DL.DDP5.1.H.264-GRP.mkv"),
        900,
    );
    write(&root.join("random.mkv"), 300);
    let r = e.svc.scan_series(s.id).await.unwrap();
    assert_eq!(r.added, 2, "{r:?}");
    assert_eq!(r.unmatched.len(), 1);
    let eps = e.svc.episodes(s.id, Some(2)).await.unwrap();
    assert!(eps.iter().all(|x| x.has_file));
    assert_eq!(eps[0].episode_file_id, eps[1].episode_file_id);
    let files = e.svc.episode_files(s.id).await.unwrap();
    assert_eq!(files.len(), 2);

    let s2 = e.svc.get_series(s.id).await.unwrap();
    // Monitored aired S01 (2) + files in S02 (2) = 4 expected, 3 have files.
    assert_eq!(s2.statistics.episode_count, 4);
    assert_eq!(s2.statistics.episode_file_count, 3);
    assert_eq!(s2.statistics.size_on_disk, (400 + 900) * MB);
    let av = e.svc.series_availability(&[1399]).await.unwrap();
    assert_eq!(av[&1399].status, AvailabilityStatus::PartiallyAvailable);
    assert_eq!(av[&1399].seasons[&2], AvailabilityStatus::Available);

    // Wanted missing: S01E02 (monitored, aired, no file).
    let wanted = e
        .svc
        .wanted_missing(&WantedQuery {
            kind: Some(CalendarKind::Episode),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(wanted.total, 1);
    assert_eq!(wanted.items[0].episode_number, Some(2));

    // Season monitoring and episode monitoring.
    let s3 = e
        .svc
        .monitor_season(SeasonMonitorRequest {
            series_id: s.id,
            season_number: 2,
            monitored: true,
            include_episodes: true,
        })
        .await
        .unwrap();
    assert!(s3.seasons[2].monitored);
    let changed = e
        .svc
        .monitor_episodes(EpisodeMonitorRequest {
            episode_ids: vec![eps[0].id],
            monitored: false,
        })
        .await
        .unwrap();
    assert!(!changed[0].monitored);

    // Calendar around "today" (2024-07-03): S02E01 (07-01, downloaded), S02E02 (07-08, has file).
    let cal = e
        .svc
        .calendar(&CalendarQuery {
            start: Some(d("2024-06-25")),
            end: Some(d("2024-07-10")),
            unmonitored: true,
        })
        .await
        .unwrap();
    assert_eq!(cal.len(), 2);
    assert!(cal.iter().all(|c| c.status == CalendarStatus::Downloaded));
    let ics = e.svc.ical(&IcalQuery::default()).await.unwrap();
    assert!(ics.starts_with("BEGIN:VCALENDAR"));
    assert!(ics.contains("SUMMARY:The Show - 2x02 - Episode 2"), "{ics}");

    // Refresh: S01E02 removed upstream, S02E03 added.
    e.meta.series.lock().unwrap().insert(
        1399,
        show(
            1399,
            vec![
                season(1, vec![episode(1, 1, "2020-01-01")]),
                season(
                    2,
                    vec![
                        episode(2, 1, "2024-07-01"),
                        episode(2, 2, "2024-07-08"),
                        episode(2, 3, "2024-07-15"),
                    ],
                ),
            ],
        ),
    );
    e.svc.refresh_series(s.id).await.unwrap();
    let all = e.svc.episodes(s.id, None).await.unwrap();
    let nums: Vec<(u32, u32)> = all.iter().map(|x| (x.season_number, x.episode_number)).collect();
    assert_eq!(nums, vec![(1, 1), (2, 1), (2, 2), (2, 3)]);
    assert!(
        all.last().unwrap().monitored,
        "new episode follows the monitored season"
    );

    // Rename preview uses season folders.
    let ren = e
        .svc
        .rename_preview(&RenamePreviewQuery {
            series_id: Some(s.id),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        ren.iter().any(|r| r
            .new_path
            .replace('\\', "/")
            .starts_with("Season 02/The Show (2020) - S02E01-02 - ")),
        "{ren:?}"
    );

    // Season pass: none.
    let sp = e
        .svc
        .season_pass(SeasonPassRequest {
            series_ids: vec![s.id],
            monitor: SeriesMonitor::None,
            monitored: Some(false),
        })
        .await
        .unwrap();
    assert!(!sp[0].monitored);
    assert!(sp[0].seasons.iter().all(|x| !x.monitored));

    let stats = e.svc.stats().await.unwrap();
    assert_eq!(stats.series.total, 1);
    assert_eq!(stats.series.episodes_with_file, 3);
    assert_eq!(stats.root_folders.len(), 2);

    e.svc.delete_series(s.id, false).await.unwrap();
    assert!(root.exists());
    assert!(
        e.svc
            .list_series(&SeriesListQuery::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn collections_import_and_settings() {
    let e = env().await;
    let mut req = add_movie_req(&e, 603);
    req.monitor = MovieMonitor::MovieAndCollection;
    e.svc.add_movie(req).await.unwrap();
    let cols = e.svc.list_collections().await.unwrap();
    assert_eq!(cols.len(), 1);
    assert!(cols[0].monitored);
    let added = e.svc.sync_collections(None).await.unwrap();
    assert_eq!(added, 1, "Reloaded is added by the monitored collection");
    let cols = e.svc.list_collections().await.unwrap();
    assert_eq!(cols[0].title, "The Matrix Collection");
    assert_eq!(cols[0].movies.len(), 2);
    assert_eq!(cols[0].missing_movies, 0);
    let c = e
        .svc
        .update_collection(
            cols[0].id,
            CollectionEdit {
                monitored: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!c.monitored);

    // Library import: an unmapped folder with a file.
    let dir = e.movies_root.path().join("Future Film (2030)");
    write(&dir.join("Future.Film.2030.2160p.WEB-DL.mkv"), 900);
    let roots = e.svc.root_folders().await.unwrap();
    let mr = roots.iter().find(|r| r.id == e.movie_root_id).unwrap();
    assert_eq!(mr.unmapped_folders.len(), 1);
    assert!(mr.free_space.is_some());
    let cands = e.svc.import_scan(e.movie_root_id, true).await.unwrap();
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].parsed_title, "Future Film");
    assert_eq!(cands[0].file_count, 1);
    assert_eq!(cands[0].suggestions[0].tmdb_id, Some(900));
    assert_eq!(cands[0].lookup_error, None);
    // Fast listing without TMDB, then matching in a batch.
    let quick = e.svc.import_scan(e.movie_root_id, false).await.unwrap();
    assert!(quick[0].suggestions.is_empty());
    let matched = e
        .svc
        .import_match(e.movie_root_id, vec![quick[0].folder.path.clone()])
        .await
        .unwrap();
    assert_eq!(matched[0].suggestions[0].tmdb_id, Some(900));
    assert_eq!(matched[0].folder.name, "Future Film (2030)");
    let outside = e.tv_root.path().join("x").to_string_lossy().into_owned();
    assert!(e.svc.import_match(e.movie_root_id, vec![outside]).await.is_err());
    let res = e
        .svc
        .library_import(LibraryImportRequest {
            root_folder_id: e.movie_root_id,
            items: vec![
                LibraryImportItem {
                    path: cands[0].folder.path.clone(),
                    tmdb_id: 900,
                    quality_profile_id: 1,
                    monitored: true,
                    minimum_availability: MinimumAvailability::Released,
                    monitor: SeriesMonitor::All,
                    series_type: SeriesType::Standard,
                    season_folder: true,
                    tags: vec![],
                },
                LibraryImportItem {
                    path: "/elsewhere/x".into(),
                    tmdb_id: 604,
                    quality_profile_id: 1,
                    monitored: true,
                    minimum_availability: MinimumAvailability::Released,
                    monitor: SeriesMonitor::All,
                    series_type: SeriesType::Standard,
                    season_folder: true,
                    tags: vec![],
                },
            ],
            search_on_add: false,
        })
        .await
        .unwrap();
    assert_eq!(res.added.len(), 1);
    assert_eq!(res.errors.len(), 1);
    let imported = e.svc.get_movie(res.added[0]).await.unwrap();
    assert_eq!(imported.path, cands[0].folder.path);
    assert!(e.svc.import_scan(e.movie_root_id, true).await.unwrap().is_empty());
    // Root folder in use cannot be deleted.
    assert!(matches!(
        e.svc.delete_root_folder(e.movie_root_id).await,
        Err(LibraryError::Conflict(_))
    ));
    assert!(matches!(
        e.svc
            .add_root_folder(
                e.movies_root.path().join("Future Film (2030)").to_str().unwrap(),
                reelix_core::MediaKind::Movie
            )
            .await,
        Err(LibraryError::Conflict(_))
    ));

    // Naming settings: validation + preview.
    let mut n = e.svc.naming().await.unwrap();
    n.movie_folder_format = "{Movie Title} ({Release Year)".into();
    assert!(matches!(
        e.svc.set_naming(n.clone()).await,
        Err(LibraryError::Validation(_))
    ));
    let p = e.svc.naming_preview(Some(n.clone())).await.unwrap();
    assert!(!p.errors.is_empty());
    n.movie_folder_format = "{Movie Title} ({Release Year})".into();
    let saved = e.svc.set_naming(n).await.unwrap();
    assert_eq!(saved.movie_folder_format, "{Movie Title} ({Release Year})");
    assert!(e.svc.naming_preview(None).await.unwrap().errors.is_empty());

    let mut mm = e.svc.media_management().await.unwrap();
    mm.file_mode = Some("999".into());
    assert!(e.svc.set_media_management(mm).await.is_err());

    // The recycle bin may not overlap a root folder.
    let root = e.svc.root_folders().await.unwrap()[0].path.clone();
    for bin in [
        root.clone(),
        format!("{root}/bin"),
        std::path::Path::new(&root)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        format!("{root}/x/../../bin"),
    ] {
        let mut mm = e.svc.media_management().await.unwrap();
        mm.recycle_bin_path = Some(bin.clone());
        assert!(
            matches!(
                e.svc.set_media_management(mm).await,
                Err(LibraryError::Validation(_))
            ),
            "{bin}"
        );
    }
    let _ = &e.tv_root;
}

#[tokio::test]
async fn search_requests_are_enqueued() {
    let e = env().await;
    let system = reelix_jobs::JobSystem::builder(e.db.clone(), EventBus::default()).build();
    e.svc.set_queue(system.queue());
    let req = SearchRequest {
        kind: LibrarySearchKind::Movie,
        ids: vec![5, 3],
        season: None,
        media: None,
    };
    let id = e.svc.request_search(req.clone()).await.unwrap().unwrap();
    let job = e.db.jobs().get(id).await.unwrap().unwrap();
    assert_eq!(job.kind, "search");
    assert_eq!(job.payload["kind"], "movie");
    assert_eq!(e.svc.request_search(req).await.unwrap(), None, "deduplicated");
    let bad = SearchRequest {
        kind: LibrarySearchKind::Season,
        ids: vec![1],
        season: None,
        media: None,
    };
    assert!(e.svc.request_search(bad).await.is_err());
}

#[tokio::test]
async fn library_import_many_series_then_scan_links_episodes() {
    let e = env().await;
    let ids = [7001u32, 7002, 7003, 7004, 7005, 7006];
    let mut items = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let mut s = show(
            *id,
            vec![season(
                1,
                vec![episode(1, 1, "2020-01-01"), episode(1, 2, "2020-01-08")],
            )],
        );
        s.title = format!("Import Show {i}");
        e.meta.series.lock().unwrap().insert(*id, s);
        let dir = e.tv_root.path().join(format!("Import Show {i}"));
        write(
            &dir.join(format!("Season 1/Import Show {i} - S01E01 - Pilot.mkv")),
            300,
        );
        write(&dir.join("Season 1/02 - Second.mkv"), 300);
        items.push(LibraryImportItem {
            path: dir.to_string_lossy().into_owned(),
            tmdb_id: *id,
            quality_profile_id: 1,
            monitored: true,
            minimum_availability: MinimumAvailability::Released,
            monitor: SeriesMonitor::All,
            series_type: SeriesType::Standard,
            season_folder: true,
            tags: vec![],
        });
    }
    // Same TMDB id picked for a second folder.
    let mut dup = items[0].clone();
    dup.path = e.tv_root.path().join("Dup").to_string_lossy().into_owned();
    items.push(dup);

    let res = e
        .svc
        .library_import(LibraryImportRequest {
            root_folder_id: e.tv_root_id,
            items,
            search_on_add: false,
        })
        .await
        .unwrap();
    assert_eq!(res.added.len(), ids.len(), "{:?}", res.errors);
    assert_eq!(res.errors.len(), 1);

    // What the queued scan job runs.
    let r = e.svc.scan_library(&[], &res.added).await.unwrap();
    assert_eq!(r.added, ids.len() * 2, "{r:?}");
    for id in &res.added {
        let s = e.svc.get_series(*id).await.unwrap();
        assert_eq!(s.statistics.episode_file_count, 2, "{}", s.title);
    }
}
