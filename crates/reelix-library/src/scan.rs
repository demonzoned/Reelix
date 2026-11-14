//! Disk scans: detect files in movie/series folders, match them, probe media info and score them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::stream::{self, StreamExt};
use reelix_api_types::library::{LibrarySearchKind, SearchRequest};

use reelix_decision::{FormatInput, QualityProfile, matching_formats, score_formats};
use reelix_import::files::{FileKind, FoundFile, SampleRules, ScanOptions, detect_sample, scan_async};
use reelix_import::matching::{ExpectedEpisode, match_parsed_episodes};
use reelix_import::mediainfo::{MediaInfo, ProbeConfig, probe};
use reelix_parser::{ParsedRelease, Quality, QualityModel, parse_path};

use crate::LibraryService;
use crate::convert::series_type;
use crate::error::Result;
use crate::repo::files::{self, FileRow, FileTable};
use crate::repo::{movies, series as repo};
use crate::service::ProfileCache;

/// Folders scanned at once (ffprobe and disk I/O dominate).
const SCAN_CONCURRENCY: usize = 4;

/// Outcome of scanning one movie/series folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unmatched: Vec<String>,
}

struct Prober {
    enabled: bool,
    cfg: ProbeConfig,
}

impl Prober {
    async fn probe(&self, p: &Path) -> Option<MediaInfo> {
        if !self.enabled {
            return None;
        }
        match probe(p, &self.cfg).await {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::debug!(path = %p.display(), error = %e, "ffprobe failed");
                None
            }
        }
    }
}

/// Scores a parsed file against the profile's custom formats.
pub(crate) fn score(
    parsed: &ParsedRelease,
    size: u64,
    profile: Option<&QualityProfile>,
    cache: &ProfileCache,
) -> (Vec<i64>, i32) {
    let mut input = FormatInput::from_parsed(parsed);
    input.size = Some(size);
    let matched = matching_formats(&cache.formats, &input);
    let ids = matched.iter().map(|f| f.id).collect();
    let s = profile.map_or(0, |p| score_formats(p, &matched));
    (ids, s)
}

/// Quality from the name, refined by the probed resolution when the name has none.
fn quality_of(parsed: &ParsedRelease, info: Option<&MediaInfo>) -> QualityModel {
    let mut q = parsed.quality;
    if q.quality == Quality::Unknown
        && let Some(m) = info
    {
        let res = m.resolution();
        q.quality = Quality::from_parts(parsed.quality.quality.source(), res);
    }
    q
}

impl LibraryService {
    async fn prober(&self) -> Result<Prober> {
        let mm = self.media_management().await?;
        Ok(Prober {
            enabled: mm.analyze_video_files,
            cfg: ProbeConfig {
                ffprobe_path: PathBuf::from(mm.ffprobe_path),
                timeout: Duration::from_secs(30),
            },
        })
    }

    async fn videos_in(&self, folder: &Path) -> Vec<FoundFile> {
        if !folder.is_dir() {
            return Vec::new();
        }
        match scan_async(folder.to_path_buf(), ScanOptions::default()).await {
            Ok(v) => v
                .into_iter()
                .filter(|f| f.kind == FileKind::Video && f.extra.is_none())
                .collect(),
            Err(e) => {
                tracing::warn!(folder = %folder.display(), error = %e, "scan failed");
                Vec::new()
            }
        }
    }

    /// Scans a movie folder; keeps the largest non-sample video as the movie file.
    pub async fn scan_movie(&self, id: i64) -> Result<ScanReport> {
        let cache = self.profiles().await?;
        let Some(m) = movies::get(&self.db, id, &cache).await? else {
            return Err(crate::LibraryError::not_found("movie", id));
        };
        let folder = PathBuf::from(&m.path);
        let mut report = ScanReport::default();
        let existing = files::list(&self.db, FileTable::Movie, Some(&[id])).await?;
        let candidate = self
            .videos_in(&folder)
            .await
            .into_iter()
            .filter(|f| !named_sample(f))
            .max_by_key(|f| f.size);

        let keep_rel = candidate
            .as_ref()
            .map(|f| f.relative.to_string_lossy().into_owned());
        for old in &existing {
            if Some(&old.relative_path) != keep_rel.as_ref() {
                files::delete(&self.db, FileTable::Movie, old.id).await?;
                report.removed += 1;
            }
        }
        if report.removed > 0 && candidate.is_none() && self.media_management().await?.unmonitor_deleted {
            sqlx::query("UPDATE movies SET monitored = 0 WHERE id = ?")
                .bind(id)
                .execute(self.db.pool())
                .await?;
        }
        if let Some(f) = candidate {
            let rel = f.relative.to_string_lossy().into_owned();
            let prev = existing.iter().find(|e| e.relative_path == rel);
            if prev.is_some_and(|p| p.size == f.size && p.media_info.is_some()) {
                // Unchanged: only rescore (profile / formats may have changed).
                let p = prev.unwrap();
                let parsed = parse_path(p.scene_name.as_deref().unwrap_or(&f.path.to_string_lossy()));
                let (ids, s) = score(&parsed, p.size, cache.profile(m.quality_profile_id), &cache);
                files::set_scores(&self.db, FileTable::Movie, p.id, &ids, s).await?;
            } else {
                let info = self.prober().await?.probe(&f.path).await;
                let source = prev
                    .and_then(|p| p.scene_name.clone())
                    .unwrap_or_else(|| f.path.to_string_lossy().into_owned());
                let parsed = parse_path(&source);
                let row = self.file_row(
                    id,
                    None,
                    rel,
                    f.size,
                    &parsed,
                    info,
                    &cache,
                    m.quality_profile_id,
                    prev,
                );
                let fid = files::upsert(&self.db, FileTable::Movie, &row).await?;
                if prev.is_some() {
                    report.updated += 1;
                } else {
                    report.added += 1;
                    self.emit("movie_file", "created", fid);
                }
            }
        }
        if report.added + report.updated + report.removed > 0 {
            self.emit("movie", "updated", id);
        }
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    fn file_row(
        &self,
        owner: i64,
        season: Option<u32>,
        rel: String,
        size: u64,
        parsed: &ParsedRelease,
        info: Option<MediaInfo>,
        cache: &ProfileCache,
        profile_id: i64,
        prev: Option<&FileRow>,
    ) -> FileRow {
        let (ids, s) = score(parsed, size, cache.profile(profile_id), cache);
        let mut languages = info.as_ref().map(|m| m.audio_languages()).unwrap_or_default();
        if languages.is_empty() {
            languages = parsed.languages.clone();
        }
        FileRow {
            id: 0,
            owner_id: owner,
            season_number: season,
            relative_path: rel,
            size,
            quality: quality_of(parsed, info.as_ref()),
            languages,
            release_group: parsed.release_group.clone(),
            edition: parsed.edition.clone(),
            scene_name: prev.and_then(|p| p.scene_name.clone()),
            original_path: prev.and_then(|p| p.original_path.clone()),
            media_info: info,
            custom_format_ids: ids,
            custom_format_score: s,
            date_added: prev.map_or_else(chrono::Utc::now, |p| p.date_added),
        }
    }

    /// Scans a series folder: matches videos to episodes (scene numbering first) and links them.
    pub async fn scan_series(&self, id: i64) -> Result<ScanReport> {
        let cache = self.profiles().await?;
        let s = self.series_row(id).await?;
        let folder = PathBuf::from(&s.path);
        let mut report = ScanReport::default();
        let eps = repo::episodes(&self.db, id, None).await?;
        let expected: Vec<ExpectedEpisode> = eps
            .iter()
            .map(|e| ExpectedEpisode {
                id: e.id,
                season: e.season_number,
                episode: e.episode_number,
                absolute: e.absolute_number,
                air_date: e.air_date,
                scene_season: e.scene_season_number,
                scene_episode: e.scene_episode_number,
                scene_absolute: e.scene_absolute_number,
            })
            .collect();
        let existing = files::list(&self.db, FileTable::Episode, Some(&[id])).await?;
        let by_rel: HashMap<&str, &FileRow> =
            existing.iter().map(|f| (f.relative_path.as_str(), f)).collect();
        let prober = self.prober().await?;
        let mut seen_files = HashSet::new();
        let mut linked: HashMap<i64, i64> = HashMap::new();

        for f in self.videos_in(&folder).await {
            if named_sample(&f) {
                continue;
            }
            let rel = f.relative.to_string_lossy().into_owned();
            let prev = by_rel.get(rel.as_str()).copied();
            let source = prev
                .and_then(|p| p.scene_name.clone())
                .unwrap_or_else(|| f.path.to_string_lossy().into_owned());
            let parsed = parse_path(&source);
            let matched = match match_parsed_episodes(&parsed, series_type(s.series_type), &expected) {
                Ok(m) if !m.episode_ids.is_empty() => m,
                _ => {
                    report.unmatched.push(rel);
                    continue;
                }
            };
            let season = eps
                .iter()
                .find(|e| e.id == matched.episode_ids[0])
                .map_or(0, |e| e.season_number);
            let fid = if prev.is_some_and(|p| p.size == f.size && p.media_info.is_some()) {
                let p = prev.unwrap();
                let (ids, sc) = score(&parsed, p.size, cache.profile(s.quality_profile_id), &cache);
                files::set_scores(&self.db, FileTable::Episode, p.id, &ids, sc).await?;
                p.id
            } else {
                let info = prober.probe(&f.path).await;
                let row = self.file_row(
                    id,
                    Some(season),
                    rel,
                    f.size,
                    &parsed,
                    info,
                    &cache,
                    s.quality_profile_id,
                    prev,
                );
                let fid = files::upsert(&self.db, FileTable::Episode, &row).await?;
                if prev.is_some() {
                    report.updated += 1;
                } else {
                    report.added += 1;
                    self.emit("episode_file", "created", fid);
                }
                fid
            };
            seen_files.insert(fid);
            for e in matched.episode_ids {
                linked.insert(e, fid);
            }
        }

        let mut deleted_eps = Vec::new();
        for old in &existing {
            if !seen_files.contains(&old.id) {
                files::delete(&self.db, FileTable::Episode, old.id).await?;
                report.removed += 1;
            }
        }
        for e in &eps {
            let new = linked.get(&e.id).copied();
            if new != e.episode_file_id {
                repo::set_episode_file(&self.db, &[e.id], new).await?;
                if new.is_none() {
                    deleted_eps.push(e.id);
                }
                self.emit("episode", "updated", e.id);
            }
        }
        if !deleted_eps.is_empty() && self.media_management().await?.unmonitor_deleted {
            repo::set_episodes_monitored(&self.db, &deleted_eps, false).await?;
        }
        if report.added + report.updated + report.removed > 0 {
            self.emit("series", "updated", id);
        }
        if !report.unmatched.is_empty() {
            tracing::info!(
                series = id,
                unmatched = report.unmatched.len(),
                "files not matched to episodes"
            );
        }
        Ok(report)
    }

    /// Scans the given movies/series; with both empty, scans the whole library.
    pub async fn scan_library(&self, movie_ids: &[i64], series_ids: &[i64]) -> Result<ScanReport> {
        let (m, s) = if movie_ids.is_empty() && series_ids.is_empty() {
            (movies::all_ids(&self.db).await?, repo::all_ids(&self.db).await?)
        } else {
            (movie_ids.to_vec(), series_ids.to_vec())
        };
        let mut total = ScanReport::default();
        let mut merge = |r: ScanReport| {
            total.added += r.added;
            total.updated += r.updated;
            total.removed += r.removed;
            total.unmatched.extend(r.unmatched);
        };
        let movies: Vec<_> = stream::iter(m)
            .map(|id| async move { (id, self.scan_movie(id).await) })
            .buffer_unordered(SCAN_CONCURRENCY)
            .collect()
            .await;
        for (id, r) in movies {
            match r {
                Ok(r) => merge(r),
                Err(e) => tracing::warn!(id, error = %e, "movie scan failed"),
            }
        }
        let series: Vec<_> = stream::iter(s)
            .map(|id| async move { (id, self.scan_series(id).await) })
            .buffer_unordered(SCAN_CONCURRENCY)
            .collect()
            .await;
        for (id, r) in series {
            match r {
                Ok(r) => merge(r),
                Err(e) => tracing::warn!(id, error = %e, "series scan failed"),
            }
        }
        Ok(total)
    }

    /// Searches the monitored movies among `movie_ids` that have no file, and the monitored
    /// missing episodes of `series_ids`.
    pub(crate) async fn search_missing_of(&self, movie_ids: &[i64], series_ids: &[i64]) -> Result<()> {
        if !movie_ids.is_empty() {
            let cache = self.profiles().await?;
            let with_file: HashSet<i64> = files::list(&self.db, FileTable::Movie, Some(movie_ids))
                .await?
                .iter()
                .map(|f| f.owner_id)
                .collect();
            let mut ids = Vec::new();
            for id in movie_ids {
                if !with_file.contains(id)
                    && movies::get(&self.db, *id, &cache)
                        .await?
                        .is_some_and(|m| m.monitored)
                {
                    ids.push(*id);
                }
            }
            if !ids.is_empty() {
                self.request_search(SearchRequest {
                    kind: LibrarySearchKind::Movie,
                    ids,
                    season: None,
                    media: None,
                })
                .await?;
            }
        }
        if !series_ids.is_empty() {
            self.request_search(SearchRequest {
                kind: LibrarySearchKind::Series,
                ids: series_ids.to_vec(),
                season: None,
                media: None,
            })
            .await?;
        }
        Ok(())
    }
}

/// A file in a library folder is skipped as a sample only when its name or folder says so.
/// The size rule of [`detect_sample`] is for downloads being imported: files already in the
/// library were put there on purpose, and old low-bitrate episodes (20 MB for 22 minutes) are
/// smaller than it allows.
fn named_sample(f: &FoundFile) -> bool {
    detect_sample(&f.path, f.size, None, None, &SampleRules::default()).is_some()
}
