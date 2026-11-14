//! Series: list/get/add/edit/delete, lookup, episodes and episode files.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

use reelix_api_types::library::{
    AddSeriesRequest, BulkDeleteRequest, EpisodeDto, EpisodeFileDto, LibrarySearchKind, LookupResult,
    NewItemMonitor, Paged, SearchRequest, SeasonDto, SeriesBulkEdit, SeriesDto, SeriesEdit, SeriesListQuery,
    SeriesMonitor, SeriesStatistics, SeriesStatus, SortDir,
};
use reelix_core::{ContentKind, MediaKind};
use reelix_db::NewJob;
use reelix_import::naming::SeriesNaming;
use reelix_metadata::{ExternalSource, MetaError, SeriesExternalId, SeriesMetadata};

use crate::content_roots;
use crate::convert::{images, ratings, series_status, year_u16};
use crate::error::{LibraryError, Result};
use crate::movies::{apply_tags, cast_of, hit_to_lookup, paginate, split_year};
use crate::repo::files::{self, FileTable};
use crate::repo::{misc, series as repo};
use crate::{LibraryService, kinds};

pub(crate) fn apply_series_metadata(s: &mut SeriesDto, meta: &SeriesMetadata) {
    s.tmdb_id = meta.tmdb_id.or(s.tmdb_id);
    s.tvdb_id = meta.tvdb_id.or(meta.external_ids.tvdb).or(s.tvdb_id);
    s.imdb_id = meta
        .imdb_id
        .clone()
        .or(meta.external_ids.imdb.clone())
        .or(s.imdb_id.take());
    s.title = meta.title.clone();
    s.original_title = meta.original_title.clone().filter(|t| *t != meta.title);
    s.sort_title = if meta.sort_title.is_empty() {
        reelix_metadata::sort_title(&meta.title)
    } else {
        meta.sort_title.clone()
    };
    s.year = meta.year;
    s.overview = meta.overview.clone();
    s.status = series_status(meta.status);
    s.network = meta.network.clone();
    s.runtime_minutes = meta.runtime_minutes.filter(|r| *r > 0);
    s.genres = meta.genres.iter().map(|g| g.name.clone()).collect();
    s.certification = meta.certification.clone();
    s.original_language = meta.original_language.clone();
    s.images = images(&meta.images);
    s.ratings = ratings(&meta.ratings);
    s.first_aired = meta.first_air_date;
    s.last_aired = meta.last_air_date;
    s.next_airing = meta.next_air_date;
    s.alternative_titles = meta.alternative_titles.iter().map(|a| a.title.clone()).collect();
}

pub(crate) fn series_naming(s: &SeriesDto) -> SeriesNaming {
    SeriesNaming {
        title: s.title.clone(),
        year: year_u16(s.year),
        tvdb_id: s.tvdb_id,
        tmdb_id: s.tmdb_id,
        imdb_id: s.imdb_id.clone(),
        tvmaze_id: None,
        series_type: crate::convert::series_type(s.series_type),
    }
}

fn blank_series() -> SeriesDto {
    SeriesDto {
        id: 0,
        tmdb_id: None,
        tvdb_id: None,
        imdb_id: None,
        title: String::new(),
        original_title: None,
        sort_title: String::new(),
        year: None,
        overview: None,
        status: SeriesStatus::Continuing,
        network: None,
        series_type: Default::default(),
        runtime_minutes: None,
        genres: Vec::new(),
        certification: None,
        original_language: None,
        images: Default::default(),
        ratings: Default::default(),
        alternative_titles: Vec::new(),
        first_aired: None,
        last_aired: None,
        next_airing: None,
        monitored: true,
        monitor_new_items: NewItemMonitor::All,
        season_folder: true,
        quality_profile_id: 0,
        root_folder_id: 0,
        path: String::new(),
        tags: Vec::new(),
        added_by_request_id: None,
        seasons: Vec::new(),
        statistics: SeriesStatistics::default(),
        cast: Vec::new(),
        added: chrono::Utc::now(),
        last_refreshed: None,
    }
}

impl LibraryService {
    pub async fn list_series(&self, q: &SeriesListQuery) -> Result<Paged<SeriesDto>> {
        // Statistics (episode counts, size) are only needed to filter or sort by them;
        // otherwise filter, sort and page light rows and fully load just the page.
        let light = q.missing.is_none() && !matches!(q.sort.as_deref(), Some("size" | "progress"));
        let mut items = if light {
            repo::load_light(&self.db).await?
        } else {
            repo::load(&self.db, None, self.today()).await?
        };
        let needle = q.q.as_deref().map(str::to_lowercase).filter(|s| !s.is_empty());
        items.retain(|s| {
            needle.as_ref().is_none_or(|n| {
                s.title.to_lowercase().contains(n)
                    || s.alternative_titles.iter().any(|t| t.to_lowercase().contains(n))
            }) && q.monitored.is_none_or(|v| s.monitored == v)
                && q.missing
                    .is_none_or(|v| (s.statistics.episode_file_count < s.statistics.episode_count) == v)
                && q.quality_profile_id.is_none_or(|v| s.quality_profile_id == v)
                && q.tag.is_none_or(|t| s.tags.contains(&t))
                && q.genre
                    .as_ref()
                    .is_none_or(|g| s.genres.iter().any(|x| x.eq_ignore_ascii_case(g)))
                && q.year.is_none_or(|y| s.year == Some(y))
                && q.status.is_none_or(|v| s.status == v)
                && q.network
                    .as_ref()
                    .is_none_or(|n| s.network.as_ref().is_some_and(|x| x.eq_ignore_ascii_case(n)))
                && q.series_type.is_none_or(|t| s.series_type == t)
        });
        let sort = q.sort.as_deref().unwrap_or("title");
        items.sort_by(|a, b| {
            let o = match sort {
                "year" => a.year.cmp(&b.year),
                "added" => a.added.cmp(&b.added),
                "size" => a.statistics.size_on_disk.cmp(&b.statistics.size_on_disk),
                "next_airing" => match (a.next_airing, b.next_airing) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                },
                "network" => a.network.cmp(&b.network),
                "status" => crate::convert::enum_str(&a.status).cmp(&crate::convert::enum_str(&b.status)),
                "rating" => a
                    .ratings
                    .vote_average
                    .partial_cmp(&b.ratings.vote_average)
                    .unwrap_or(Ordering::Equal),
                "progress" => a
                    .statistics
                    .percent_of_episodes
                    .partial_cmp(&b.statistics.percent_of_episodes)
                    .unwrap_or(Ordering::Equal),
                _ => Ordering::Equal,
            };
            o.then_with(|| a.sort_title.cmp(&b.sort_title))
        });
        if q.dir == Some(SortDir::Desc) {
            items.reverse();
        }
        let mut page = paginate(items, q.page, q.page_size);
        if light {
            page.items = repo::hydrate(&self.db, page.items, self.today()).await?;
        }
        Ok(page)
    }

    pub(crate) async fn series_row(&self, id: i64) -> Result<SeriesDto> {
        repo::load(&self.db, Some(&[id]), self.today())
            .await?
            .pop()
            .ok_or_else(|| LibraryError::not_found("series", id))
    }

    /// One series including cast.
    pub async fn get_series(&self, id: i64) -> Result<SeriesDto> {
        let mut s = self.series_row(id).await?;
        s.cast = misc::credits(&self.db, MediaKind::Series, id).await?;
        Ok(s)
    }

    pub async fn series_by_tmdb(&self, tmdb_id: u32) -> Result<Option<SeriesDto>> {
        match repo::id_by_tmdb(&self.db, tmdb_id).await? {
            Some(id) => Ok(Some(self.get_series(id).await?)),
            None => Ok(None),
        }
    }

    /// Adds a series from TMDB with all seasons/episodes, applies the monitor option,
    /// queues a disk scan and (optionally) a search.
    pub async fn add_series(&self, req: AddSeriesRequest) -> Result<SeriesDto> {
        self.add_series_inner(req, true).await
    }

    /// [`Self::add_series`]; with `scan` false the caller queues the disk scan itself.
    pub(crate) async fn add_series_inner(&self, req: AddSeriesRequest, scan: bool) -> Result<SeriesDto> {
        self.check_profile(req.quality_profile_id).await?;
        let root = self.require_root(req.root_folder_id, MediaKind::Series).await?;
        let ext = match (req.tmdb_id, req.tvdb_id) {
            (Some(t), _) => {
                if repo::id_by_tmdb(&self.db, t).await?.is_some() {
                    return Err(LibraryError::Conflict(format!(
                        "series with TMDB id {t} is already in the library"
                    )));
                }
                SeriesExternalId::Tmdb(t)
            }
            (None, Some(t)) => {
                if repo::id_by_tvdb(&self.db, t).await?.is_some() {
                    return Err(LibraryError::Conflict(format!(
                        "series with TVDB id {t} is already in the library"
                    )));
                }
                SeriesExternalId::Tvdb(t)
            }
            (None, None) => return Err(LibraryError::validation("tmdb_id or tvdb_id is required")),
        };
        let meta = series_metadata(self.metadata.as_ref(), ext, req.tvdb_id).await?;
        let Some(tmdb_id) = meta.tmdb_id else {
            return Err(LibraryError::validation("metadata has no TMDB id"));
        };
        if repo::id_by_tmdb(&self.db, tmdb_id).await?.is_some() {
            return Err(LibraryError::Conflict(format!(
                "series with TMDB id {tmdb_id} is already in the library"
            )));
        }
        // The TVDB id is checked too: a request may pair a new TMDB id with the TVDB id of a show
        // already in the library, which would add that show a second time.
        if let Some(tvdb_id) = meta.tvdb_id.or(meta.external_ids.tvdb)
            && repo::id_by_tvdb(&self.db, tvdb_id).await?.is_some()
        {
            return Err(LibraryError::Conflict(format!(
                "series with TVDB id {tvdb_id} is already in the library"
            )));
        }
        let mut s = blank_series();
        apply_series_metadata(&mut s, &meta);
        s.tmdb_id = Some(tmdb_id);
        s.quality_profile_id = req.quality_profile_id;
        s.root_folder_id = root.id;
        s.series_type = req.series_type;
        s.season_folder = req.season_folder;
        s.monitor_new_items = req.monitor_new_items;
        s.monitored =
            req.monitor != SeriesMonitor::None || req.seasons.as_ref().is_some_and(|v| !v.is_empty());
        s.tags = req.tags.clone();
        s.tags.sort_unstable();
        s.tags.dedup();
        s.added_by_request_id = req.added_by_request_id;
        s.last_refreshed = Some(chrono::Utc::now());
        s.path = match req.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => {
                let p = crate::root_folders::normalize(p);
                content_roots::ensure_inside(&p, [root.path.as_str()])?;
                p
            }
            None => {
                let folder = self.naming_config().await?.series_folder(&series_naming(&s))?;
                Path::new(&root.path).join(folder).to_string_lossy().into_owned()
            }
        };
        let id = repo::insert(&self.db, &s).await?;
        let alts: Vec<(String, Option<String>)> = meta
            .alternative_titles
            .iter()
            .map(|a| (a.title.clone(), a.country.clone()))
            .collect();
        misc::set_alt_titles(&self.db, MediaKind::Series, id, &alts).await?;
        misc::set_credits(&self.db, MediaKind::Series, id, &cast_of(&meta.credits)).await?;
        self.sync_episodes(id, &meta, true).await?;
        self.apply_monitor(id, req.monitor, req.seasons.as_deref())
            .await?;
        self.emit("series", "created", id);
        tracing::info!(id, tmdb_id, title = %s.title, "series added");

        if scan {
            self.queue_scan(&[], &[id]).await?;
        }
        if req.search_on_add && s.monitored {
            self.request_search(SearchRequest {
                kind: LibrarySearchKind::Series,
                ids: vec![id],
                season: None,
                media: None,
            })
            .await?;
        }
        if req.search_cutoff_unmet {
            self.request_search(SearchRequest {
                kind: LibrarySearchKind::Cutoff,
                ids: vec![id],
                season: None,
                media: Some(reelix_api_types::library::CalendarKind::Episode),
            })
            .await?;
        }
        self.get_series(id).await
    }

    /// Creates/updates seasons and episodes from metadata; removes episodes (without files)
    /// that no longer exist. On refresh, new seasons follow `monitor_new_items`.
    pub(crate) async fn sync_episodes(
        &self,
        series_id: i64,
        meta: &SeriesMetadata,
        is_new: bool,
    ) -> Result<()> {
        let s = self.series_row(series_id).await?;
        let new_monitored = is_new || (s.monitored && s.monitor_new_items == NewItemMonitor::All);
        let mut season_monitor: HashMap<u32, bool> =
            s.seasons.iter().map(|x| (x.season_number, x.monitored)).collect();
        let mut keep_seasons = Vec::new();
        for season in &meta.seasons {
            let n = season.season_number;
            keep_seasons.push(n);
            let monitored = *season_monitor.entry(n).or_insert(new_monitored && n > 0);
            repo::upsert_season(
                &self.db,
                series_id,
                &SeasonDto {
                    season_number: n,
                    monitored,
                    title: season.name.clone(),
                    overview: season.overview.clone(),
                    poster: season.poster.clone(),
                    air_date: season.air_date,
                    statistics: Default::default(),
                },
            )
            .await?;
        }
        let existing = repo::episodes(&self.db, series_id, None).await?;
        let mut seen = std::collections::HashSet::new();
        for e in meta.episodes() {
            let monitored = season_monitor.get(&e.season_number).copied().unwrap_or(false);
            let dto = EpisodeDto {
                id: 0,
                series_id,
                season_number: e.season_number,
                episode_number: e.episode_number,
                absolute_number: e.absolute_number,
                scene_season_number: None,
                scene_episode_number: None,
                scene_absolute_number: None,
                title: e.title.clone(),
                overview: e.overview.clone(),
                air_date: e.air_date,
                air_date_utc: None,
                runtime_minutes: e.runtime_minutes,
                still: e.still.clone(),
                finale: e.finale.clone(),
                monitored,
                has_file: false,
                episode_file_id: None,
                episode_file: None,
            };
            let (id, inserted) = repo::upsert_episode(&self.db, &dto).await?;
            seen.insert(id);
            if inserted && !is_new {
                self.emit("episode", "created", id);
            }
        }
        let stale: Vec<i64> = existing
            .iter()
            .filter(|e| !seen.contains(&e.id) && e.episode_file_id.is_none())
            .map(|e| e.id)
            .collect();
        repo::delete_episodes(&self.db, &stale).await?;
        let mut keep = keep_seasons;
        keep.extend(
            existing
                .iter()
                .filter(|e| e.episode_file_id.is_some())
                .map(|e| e.season_number),
        );
        repo::delete_seasons_except(&self.db, series_id, &keep).await?;
        Ok(())
    }

    pub async fn update_series(&self, id: i64, edit: SeriesEdit) -> Result<SeriesDto> {
        self.edit_series(&[id], &edit, true).await?;
        self.get_series(id).await
    }

    pub async fn bulk_update_series(&self, req: SeriesBulkEdit) -> Result<Vec<SeriesDto>> {
        if req.ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut edit = req.edit;
        edit.path = None;
        self.edit_series(&req.ids, &edit, false).await?;
        repo::load(&self.db, Some(&req.ids), self.today()).await
    }

    async fn edit_series(&self, ids: &[i64], e: &SeriesEdit, single: bool) -> Result<()> {
        if let Some(p) = e.quality_profile_id {
            self.check_profile(p).await?;
        }
        let new_root = match e.root_folder_id {
            Some(r) => Some(self.require_root(r, MediaKind::Series).await?),
            None => None,
        };
        let list = repo::load(&self.db, Some(ids), self.today()).await?;
        if single && list.is_empty() {
            return Err(LibraryError::not_found("series", ids[0]));
        }
        for mut s in list {
            let old_path = s.path.clone();
            if let Some(v) = e.monitored {
                s.monitored = v;
            }
            if let Some(v) = e.quality_profile_id {
                s.quality_profile_id = v;
            }
            if let Some(v) = e.series_type {
                s.series_type = v;
            }
            if let Some(v) = e.season_folder {
                s.season_folder = v;
            }
            if let Some(v) = e.monitor_new_items {
                s.monitor_new_items = v;
            }
            if let Some(r) = &new_root {
                s.root_folder_id = r.id;
                let name = Path::new(&old_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| s.title.clone());
                s.path = Path::new(&r.path).join(name).to_string_lossy().into_owned();
            }
            if let Some(p) = e.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
                if !Path::new(p).is_absolute() {
                    return Err(LibraryError::validation("path must be absolute"));
                }
                s.path = crate::root_folders::normalize(p);
            }
            if let Some(t) = &e.tags {
                apply_tags(&mut s.tags, t, e.tag_mode);
            }
            if s.path != old_path {
                content_roots::ensure_inside_kinds(&self.db, &s.path, &[ContentKind::Series]).await?;
            }
            if e.move_files && s.path != old_path {
                self.move_media_folder(Path::new(&old_path), Path::new(&s.path))
                    .await?;
            }
            repo::save(&self.db, s.id, &s).await?;
            self.emit("series", "updated", s.id);
        }
        Ok(())
    }

    pub async fn delete_series(&self, id: i64, delete_files: bool) -> Result<()> {
        let s = self.series_row(id).await?;
        if delete_files {
            self.remove_path(Path::new(&s.path)).await?;
        }
        repo::delete(&self.db, id).await?;
        self.emit("series", "deleted", id);
        tracing::info!(id, title = %s.title, delete_files, "series deleted");
        Ok(())
    }

    pub async fn bulk_delete_series(&self, req: BulkDeleteRequest) -> Result<u32> {
        let mut n = 0;
        for id in req.ids {
            match self.delete_series(id, req.delete_files).await {
                Ok(()) => n += 1,
                Err(LibraryError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }

    /// TMDB series search with in-library flags (`tmdb:`, `tvdb:`, `imdb:` prefixes).
    pub async fn lookup_series(&self, term: &str) -> Result<Vec<LookupResult>> {
        let term = term.trim();
        if term.is_empty() {
            return Ok(Vec::new());
        }
        let lower = term.to_ascii_lowercase();
        let hits = if let Some(id) = lower
            .strip_prefix("tmdb:")
            .and_then(|s| s.trim().parse::<u32>().ok())
        {
            let m = self.metadata.series(SeriesExternalId::Tmdb(id)).await?;
            vec![series_hit(&m)]
        } else if let Some(id) = lower.strip_prefix("tvdb:").map(str::trim) {
            self.metadata
                .find(ExternalSource::Tvdb, id, MediaKind::Series)
                .await?
        } else if let Some(id) = lower.strip_prefix("imdb:").map(str::trim) {
            self.metadata
                .find(ExternalSource::Imdb, id, MediaKind::Series)
                .await?
        } else {
            let (title, year) = split_year(term);
            let hits = self.metadata.search(&title, MediaKind::Series, year).await?;
            if hits.is_empty() && year.is_some() {
                self.metadata.search(term, MediaKind::Series, None).await?
            } else {
                hits
            }
        };
        let (tmdb, tvdb) = repo::id_index(&self.db).await?;
        Ok(hits
            .iter()
            .map(|h| {
                let id = h
                    .tmdb_id
                    .and_then(|t| tmdb.get(&t).copied())
                    .or_else(|| h.tvdb_id.and_then(|t| tvdb.get(&t).copied()));
                hit_to_lookup(h, id)
            })
            .collect())
    }

    /// Episodes of a series (optionally one season) with their files.
    pub async fn episodes(&self, series_id: i64, season: Option<u32>) -> Result<Vec<EpisodeDto>> {
        let s = self.series_row(series_id).await?;
        let mut eps = repo::episodes(&self.db, series_id, season).await?;
        let files: HashMap<i64, EpisodeFileDto> = self
            .episode_files_of(&s)
            .await?
            .into_iter()
            .map(|f| (f.file.id, f))
            .collect();
        for e in &mut eps {
            e.episode_file = e.episode_file_id.and_then(|id| files.get(&id).cloned());
        }
        Ok(eps)
    }

    pub async fn episode_files(&self, series_id: i64) -> Result<Vec<EpisodeFileDto>> {
        let s = self.series_row(series_id).await?;
        self.episode_files_of(&s).await
    }

    pub(crate) async fn episode_files_of(&self, s: &SeriesDto) -> Result<Vec<EpisodeFileDto>> {
        let profiles = self.profiles().await?;
        let profile = profiles.profile(s.quality_profile_id);
        let map = repo::file_episode_map(&self.db, Some(s.id)).await?;
        let rows = files::list(&self.db, FileTable::Episode, Some(&[s.id])).await?;
        Ok(rows
            .iter()
            .map(|f| EpisodeFileDto {
                series_id: s.id,
                season_number: f.season_number.unwrap_or(0),
                episode_ids: map.get(&f.id).cloned().unwrap_or_default(),
                file: f.to_dto(&s.path, profile, &profiles.format_names),
            })
            .collect())
    }

    /// Deletes an episode file from disk (recycle bin when configured) and the database.
    pub async fn delete_episode_file(&self, file_id: i64) -> Result<()> {
        let f = files::get(&self.db, FileTable::Episode, file_id)
            .await?
            .ok_or_else(|| LibraryError::not_found("episode file", file_id))?;
        let path: Option<String> = sqlx::query_scalar("SELECT path FROM series WHERE id = ?")
            .bind(f.owner_id)
            .fetch_optional(self.db.pool())
            .await?;
        if let Some(p) = path {
            self.remove_path(&Path::new(&p).join(&f.relative_path)).await?;
        }
        let eps = repo::file_episode_map(&self.db, Some(f.owner_id))
            .await?
            .remove(&file_id)
            .unwrap_or_default();
        repo::set_episode_file(&self.db, &eps, None).await?;
        if self.media_management().await?.unmonitor_deleted {
            repo::set_episodes_monitored(&self.db, &eps, false).await?;
        }
        files::delete(&self.db, FileTable::Episode, file_id).await?;
        self.emit("episode_file", "deleted", file_id);
        for e in eps {
            self.emit("episode", "updated", e);
        }
        self.emit("series", "updated", f.owner_id);
        Ok(())
    }

    pub async fn queue_refresh_series(&self, id: i64) -> Result<Option<i64>> {
        self.series_row(id).await?;
        self.enqueue(
            NewJob::new(
                kinds::REFRESH_SERIES,
                serde_json::json!({ "series_ids": [id], "manual": true }),
            )
            .priority(10)
            .dedupe(format!("refresh_series:{id}")),
        )
        .await
    }

    pub async fn queue_rescan_series(&self, id: i64) -> Result<Option<i64>> {
        self.series_row(id).await?;
        self.queue_scan(&[], &[id]).await
    }
}

pub(crate) fn series_hit(m: &SeriesMetadata) -> reelix_metadata::SearchHit {
    reelix_metadata::SearchHit {
        kind: MediaKind::Series,
        tmdb_id: m.tmdb_id,
        tvdb_id: m.tvdb_id,
        imdb_id: m.imdb_id.clone(),
        title: m.title.clone(),
        original_title: m.original_title.clone(),
        year: m.year,
        release_date: m.first_air_date,
        overview: m.overview.clone(),
        poster: m.images.poster.clone(),
        backdrop: m.images.backdrop.clone(),
        genre_ids: m.genres.iter().map(|g| g.id).collect(),
        original_language: m.original_language.clone(),
        vote_average: m.ratings.vote_average,
        vote_count: m.ratings.vote_count,
        popularity: m.ratings.popularity,
        adult: false,
    }
}

/// A series' metadata by `ext`. When that is a TMDB id the source cannot look up (open data
/// without a Wikidata link answers `Unsupported`), the TVDB id is tried next.
///
/// `NotFound` is not retried: from TMDB it means the entry was removed, and a refresh marks the
/// series deleted rather than moving it to whatever the TVDB id finds. The TVDB result keeps the
/// TMDB id only when it has no TMDB link of its own, and that pairing is unverified, so it is
/// logged; a result linked to another TMDB entry is refused, as the two ids name different shows.
pub(crate) async fn series_metadata(
    metadata: &dyn crate::MetadataSource,
    ext: SeriesExternalId,
    tvdb: Option<u32>,
) -> std::result::Result<SeriesMetadata, MetaError> {
    match (metadata.series(ext.clone()).await, ext, tvdb) {
        (Err(MetaError::Unsupported(why)), SeriesExternalId::Tmdb(t), Some(tv)) => {
            let mut m = metadata.series(SeriesExternalId::Tvdb(tv)).await?;
            match m.tmdb_id.or(m.external_ids.tmdb) {
                None => {
                    tracing::warn!(
                        tmdb_id = t,
                        tvdb_id = tv,
                        title = %m.title,
                        "series found by TVDB id only; its TMDB id is taken from the request and is unverified"
                    );
                    m.tmdb_id = Some(t);
                    m.external_ids.tmdb = Some(t);
                }
                Some(found) if found == t => {
                    m.tmdb_id = Some(t);
                    m.external_ids.tmdb = Some(t);
                }
                Some(found) => {
                    tracing::warn!(
                        tmdb_id = t,
                        tvdb_id = tv,
                        linked_tmdb_id = found,
                        "the TVDB id belongs to another TMDB entry; not used"
                    );
                    return Err(MetaError::Unsupported(why));
                }
            }
            Ok(m)
        }
        (result, _, _) => result,
    }
}
