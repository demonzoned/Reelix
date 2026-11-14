//! Metadata refresh for movies and series.

use reelix_api_types::library::{MovieStatus, RescanAfterRefresh, SeriesStatus};
use reelix_core::MediaKind;
use reelix_metadata::{MetaError, SeriesExternalId};

use crate::LibraryService;
use crate::error::{LibraryError, Result};
use crate::movies::{apply_movie_metadata, cast_of};
use crate::repo::misc::{self, CollectionRow};
use crate::repo::{movies, series};
use crate::series::apply_series_metadata;

impl LibraryService {
    /// Refreshes one movie from TMDB. A movie removed from TMDB is marked `deleted`.
    pub async fn refresh_movie(&self, id: i64) -> Result<()> {
        let profiles = self.profiles().await?;
        let mut m = movies::get(&self.db, id, &profiles)
            .await?
            .ok_or_else(|| LibraryError::not_found("movie", id))?;
        let meta = match self.metadata.movie(m.tmdb_id).await {
            Ok(meta) => meta,
            Err(MetaError::NotFound) => {
                m.status = MovieStatus::Deleted;
                m.last_refreshed = Some(chrono::Utc::now());
                movies::save(&self.db, id, &m).await?;
                self.emit("movie", "updated", id);
                tracing::warn!(id, tmdb_id = m.tmdb_id, "movie no longer exists on TMDB");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        apply_movie_metadata(&mut m, &meta, self.today());
        m.last_refreshed = Some(chrono::Utc::now());
        if let Some(c) = &meta.collection {
            let cid = match misc::collection_by_tmdb(&self.db, c.tmdb_id).await? {
                Some(row) => row.id,
                None => {
                    misc::upsert_collection(
                        &self.db,
                        &CollectionRow {
                            id: 0,
                            tmdb_id: c.tmdb_id,
                            title: c.name.clone(),
                            overview: None,
                            images: reelix_api_types::library::ImagesDto {
                                poster: c.poster.clone(),
                                backdrop: c.backdrop.clone(),
                                logo: None,
                            },
                            parts: Vec::new(),
                            monitored: false,
                            quality_profile_id: m.quality_profile_id,
                            root_folder_id: m.root_folder_id,
                            minimum_availability: m.minimum_availability,
                            search_on_add: false,
                            tags: m.tags.clone(),
                            last_synced: None,
                        },
                    )
                    .await?
                }
            };
            m.collection = Some(reelix_api_types::library::CollectionRefDto {
                id: cid,
                tmdb_id: c.tmdb_id,
                title: c.name.clone(),
            });
        } else {
            m.collection = None;
        }
        movies::save(&self.db, id, &m).await?;
        let alts: Vec<(String, Option<String>)> = meta
            .alternative_titles
            .iter()
            .map(|a| (a.title.clone(), a.country.clone()))
            .collect();
        misc::set_alt_titles(&self.db, MediaKind::Movie, id, &alts).await?;
        misc::set_credits(&self.db, MediaKind::Movie, id, &cast_of(&meta.credits)).await?;
        self.emit("movie", "updated", id);
        Ok(())
    }

    /// Refreshes one series: metadata, seasons and episodes (added/updated/removed).
    pub async fn refresh_series(&self, id: i64) -> Result<()> {
        let mut s = self.series_row(id).await?;
        let tmdb = s.tmdb_id.unwrap_or_default();
        let meta = match crate::series::series_metadata(
            self.metadata.as_ref(),
            SeriesExternalId::Tmdb(tmdb),
            s.tvdb_id,
        )
        .await
        {
            Ok(m) => m,
            Err(MetaError::NotFound) => {
                s.status = SeriesStatus::Deleted;
                s.last_refreshed = Some(chrono::Utc::now());
                series::save(&self.db, id, &s).await?;
                self.emit("series", "updated", id);
                tracing::warn!(id, tmdb_id = tmdb, "series no longer exists on TMDB");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        apply_series_metadata(&mut s, &meta);
        s.last_refreshed = Some(chrono::Utc::now());
        series::save(&self.db, id, &s).await?;
        let alts: Vec<(String, Option<String>)> = meta
            .alternative_titles
            .iter()
            .map(|a| (a.title.clone(), a.country.clone()))
            .collect();
        misc::set_alt_titles(&self.db, MediaKind::Series, id, &alts).await?;
        misc::set_credits(&self.db, MediaKind::Series, id, &cast_of(&meta.credits)).await?;
        self.sync_episodes(id, &meta, false).await?;
        self.emit("series", "updated", id);
        Ok(())
    }

    /// Refreshes movies (`None` = all), then rescans per the media-management setting.
    /// Per-item failures are logged; returns the number of failures.
    pub async fn refresh_movies(&self, ids: Option<Vec<i64>>, manual: bool) -> Result<usize> {
        let ids = match ids {
            Some(ids) if !ids.is_empty() => ids,
            _ => movies::all_ids(&self.db).await?,
        };
        let mut failed = 0;
        for id in &ids {
            if let Err(e) = self.refresh_movie(*id).await {
                if matches!(e, LibraryError::Metadata(MetaError::NotConfigured(_))) {
                    return Err(e);
                }
                failed += 1;
                tracing::warn!(id, error = %e, "movie refresh failed");
            }
        }
        if self.rescan_after(manual).await? {
            for id in &ids {
                if let Err(e) = self.scan_movie(*id).await {
                    tracing::warn!(id, error = %e, "movie rescan failed");
                }
            }
        }
        Ok(failed)
    }

    pub async fn refresh_all_series(&self, ids: Option<Vec<i64>>, manual: bool) -> Result<usize> {
        let ids = match ids {
            Some(ids) if !ids.is_empty() => ids,
            _ => series::all_ids(&self.db).await?,
        };
        let mut failed = 0;
        for id in &ids {
            if let Err(e) = self.refresh_series(*id).await {
                if matches!(e, LibraryError::Metadata(MetaError::NotConfigured(_))) {
                    return Err(e);
                }
                failed += 1;
                tracing::warn!(id, error = %e, "series refresh failed");
            }
        }
        if self.rescan_after(manual).await? {
            for id in &ids {
                if let Err(e) = self.scan_series(*id).await {
                    tracing::warn!(id, error = %e, "series rescan failed");
                }
            }
        }
        Ok(failed)
    }

    async fn rescan_after(&self, manual: bool) -> Result<bool> {
        Ok(match self.media_management().await?.rescan_after_refresh {
            RescanAfterRefresh::Always => true,
            RescanAfterRefresh::AfterManual => manual,
            RescanAfterRefresh::Never => false,
        })
    }
}
