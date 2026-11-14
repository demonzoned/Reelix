//! Wiring for domain modules (library, providers, requests, pipeline, …).
//!
//! Startup runs in three phases so services, job handlers and the API can reference each other:
//! 1. [`Modules::build`] creates every service (no job queue yet),
//! 2. [`Modules::job_handlers`] registers job handlers and scheduled tasks,
//! 3. [`Modules::attach`] hands services the job queue and registers them on the API state.
//!
//! Each domain adds one field and one short block per phase.

use std::path::PathBuf;

use reelix_api::AppState;
use reelix_core::bus::EventBus;
use reelix_db::Db;
use reelix_db::secrets::SecretBox;
use reelix_jobs::{JobQueue, JobSystemBuilder};

/// Shared dependencies available to every module.
#[derive(Clone)]
pub struct Deps {
    pub db: Db,
    pub events: EventBus,
    pub secrets: SecretBox,
    pub http: reqwest::Client,
    pub data_dir: PathBuf,
    /// Resolved bootstrap configuration (paths, config file, server settings).
    pub config: crate::config::Config,
}

/// Every domain service, built once at startup.
pub struct Modules {
    #[allow(dead_code)]
    deps: Deps,
    pub providers: std::sync::Arc<reelix_providers::ProviderService>,
    pub library: std::sync::Arc<reelix_library::LibraryService>,
    pub books: std::sync::Arc<reelix_books::BooksService>,
    pub music: std::sync::Arc<reelix_music::MusicService>,
    pub radio: std::sync::Arc<reelix_radio::RadioService>,
    /// Radio's Audiobookshelf podcast library reader (also a health source).
    pub radio_library: std::sync::Arc<crate::integrations::radio_library::AbsRadioLibrary>,
    pub requests: std::sync::Arc<reelix_requests::RequestService>,
    pub integrations: std::sync::Arc<crate::integrations::IntegrationsService>,
    pub search: std::sync::Arc<reelix_pipeline::search::SearchService>,
    pub activity: std::sync::Arc<reelix_pipeline::ActivityService>,
    pub maintenance: std::sync::Arc<crate::maintenance::MaintenanceService>,
    pub migrate: std::sync::Arc<reelix_migrate::MigrateService>,
}

impl Modules {
    pub async fn build(deps: Deps) -> anyhow::Result<Self> {
        // providers
        let providers = reelix_providers::ProviderService::new(
            deps.db.clone(),
            deps.secrets.clone(),
            deps.events.clone(),
            deps.http.clone(),
            reelix_providers::ProviderOptions::new(&deps.data_dir),
        )
        .await?;
        // library (TMDB resolved per call from the providers' metadata settings; without a TMDB key,
        // open data: films from Wikidata and Wikipedia, series from TVmaze, no key needed)
        let library_metadata;
        let library = {
            let p = providers.clone();
            let open: std::sync::Arc<dyn reelix_library::MetadataSource> =
                std::sync::Arc::new(reelix_metadata::OpenData::new(
                    deps.http.clone(),
                    reelix_metadata::OpenDataOptions {
                        language: providers.metadata_config().language,
                        ..Default::default()
                    },
                ));
            let metadata = reelix_library::DynMetadata::new(move || {
                let p = p.clone();
                let open = open.clone();
                async move {
                    Ok(p.metadata()
                        .map(|t| t as std::sync::Arc<dyn reelix_library::MetadataSource>)
                        .unwrap_or(open))
                }
            });
            let metadata = std::sync::Arc::new(metadata);
            library_metadata = metadata.clone();
            std::sync::Arc::new(reelix_library::LibraryService::new(
                deps.db.clone(),
                deps.events.clone(),
                metadata,
            ))
        };
        // books (Open Library metadata, no API key); the client is shared with book Discover rows
        let open_library = reelix_metadata::OpenLibrary::new(deps.http.clone(), Default::default());
        let books = std::sync::Arc::new(reelix_books::BooksService::new(reelix_books::BooksDeps {
            db: deps.db.clone(),
            events: deps.events.clone(),
            http: deps.http.clone(),
            library: library.clone(),
            metadata: std::sync::Arc::new(open_library.clone()),
        }));
        // music (MusicBrainz + Cover Art Archive metadata, no API key)
        let music = std::sync::Arc::new(reelix_music::MusicService::new(reelix_music::MusicDeps {
            db: deps.db.clone(),
            events: deps.events.clone(),
            http: deps.http.clone(),
            library: library.clone(),
            metadata: std::sync::Arc::new(reelix_metadata::MusicBrainz::new(
                deps.http.clone(),
                Default::default(),
            )),
        }));
        // radio: providers in link-recognition order (specific broadcasters and directories
        // first, the generic podcast feed last); the Audiobookshelf podcast library reader
        // (media servers' Audiobookshelf rows) is attached with `RadioService::set_library_sync`
        let radio = std::sync::Arc::new(reelix_radio::RadioService::new(reelix_radio::RadioDeps {
            db: deps.db.clone(),
            events: deps.events.clone(),
            http: deps.http.clone(),
            providers: {
                use reelix_radio::providers::{
                    AppleProvider, ArchiveProvider, BbcProvider, PodcastIndexProvider, RssProvider,
                };
                use std::sync::Arc;
                // Feeds read for Discover and requests reach public addresses only.
                let public_http = reelix_providers::public_http_client()?;
                reelix_radio::ProviderRegistry::default()
                    .with(Arc::new(BbcProvider::new(deps.http.clone())))
                    .with(Arc::new(
                        AppleProvider::new(deps.http.clone()).with_public_client(public_http.clone()),
                    ))
                    .with(Arc::new(PodcastIndexProvider::new(deps.http.clone())))
                    .with(Arc::new(ArchiveProvider::new(deps.http.clone())))
                    .with(Arc::new(
                        RssProvider::new(deps.http.clone()).with_public_client(public_http),
                    ))
            },
            library_sync: None,
        }));
        // downloaded artwork and logos under <data_dir>/radio/; provider credentials encrypted
        radio.set_data_dir(deps.data_dir.clone());
        radio.set_secrets(deps.secrets.clone());
        let radio_library = {
            let client_id = deps
                .db
                .settings()
                .get::<String>("providers.client_id")
                .await?
                .unwrap_or_else(|| "reelix".into());
            let p = providers.clone();
            let sync = crate::integrations::radio_library::AbsRadioLibrary::new(
                std::sync::Arc::new(move || p.media_servers().into_iter().map(|(row, _)| row).collect()),
                reelix_mediaserver::BuildContext::new(deps.http.clone(), client_id),
                std::sync::Arc::downgrade(&radio),
            );
            radio.set_library_sync(sync.clone());
            sync
        };
        // requests (metadata, media servers and notifications via providers; library via LibraryAdapter)
        let requests = {
            use reelix_requests::gateway::{ResolvingSource, adapters};
            use reelix_requests::mediaserver::{BuildContext, plextv::PlexTv};
            use std::sync::Arc;
            let client_id = deps
                .db
                .settings()
                .get::<String>("providers.client_id")
                .await?
                .unwrap_or_else(|| "reelix".into());
            let p = providers.clone();
            let metadata = ResolvingSource(Arc::new(move || {
                p.metadata()
                    .map(|t| t as Arc<dyn reelix_requests::DiscoverySource>)
                    .map_err(|_| reelix_requests::metadata::MetaError::NotConfigured("TMDB"))
            }));
            let p = providers.clone();
            let media_servers = adapters::FnDirectory {
                servers: Arc::new(move || {
                    p.media_servers()
                        .into_iter()
                        .filter(|(row, _)| row.enabled)
                        .filter_map(|(row, server)| {
                            let kind = match row.implementation.as_str() {
                                "jellyfin" => reelix_requests::MediaServerKind::Jellyfin,
                                "emby" => reelix_requests::MediaServerKind::Emby,
                                "plex" => reelix_requests::MediaServerKind::Plex,
                                _ => return None,
                            };
                            Some(reelix_requests::MediaServerEntry {
                                id: row.id,
                                name: row.name,
                                kind,
                                server,
                            })
                        })
                        .collect()
                }),
                plex_tv: PlexTv::new(&BuildContext::new(deps.http.clone(), client_id)),
            };
            let p = providers.clone();
            let notifications = adapters::FnNotifications(Arc::new(move |event, _recipients| {
                let p = p.clone();
                Box::pin(async move { p.notify(&event).await })
            }));
            Arc::new(reelix_requests::RequestService::new(
                reelix_requests::RequestDeps {
                    db: deps.db.clone(),
                    events: deps.events.clone(),
                    secrets: deps.secrets.clone(),
                    library: Arc::new(crate::adapters::LibraryAdapter {
                        library: library.clone(),
                        db: deps.db.clone(),
                        books: books.clone(),
                        music: music.clone(),
                        open_library: open_library.clone(),
                        listenbrainz: reelix_metadata::ListenBrainz::new(
                            deps.http.clone(),
                            Default::default(),
                        ),
                        radio: radio.clone(),
                    }),
                    media_servers: Arc::new(media_servers),
                    metadata: Arc::new(metadata),
                    notifications: Arc::new(notifications),
                    http: Some(deps.http.clone()),
                },
            ))
        };
        // integrations (import list sync, notification bridge, health watch, media server availability)
        let integrations = {
            let p = providers.clone();
            let metadata = reelix_library::DynMetadata::new(move || {
                let p = p.clone();
                async move {
                    p.metadata()
                        .map(|t| t as std::sync::Arc<dyn reelix_library::MetadataSource>)
                        .map_err(|_| reelix_library::MetaError::NotConfigured("TMDB"))
                }
            });
            crate::integrations::IntegrationsService::new(
                deps.db.clone(),
                deps.events.clone(),
                library.clone(),
                std::sync::Arc::new(metadata),
                crate::integrations::Sources::from_providers(&providers),
            )
        };
        // pipeline: search / grab / RSS / delayed releases
        let search = std::sync::Arc::new(reelix_pipeline::search::SearchService::new(
            deps.db.clone(),
            deps.events.clone(),
            providers.clone(),
            library.clone(),
            reelix_pipeline::search::SearchOptions::default(),
        ));
        // pipeline: download monitoring, import, queue/history, manual import
        let activity = std::sync::Arc::new(reelix_pipeline::ActivityService::new(
            deps.db.clone(),
            deps.events.clone(),
            library.clone(),
            providers.clone(),
        ));
        // books / music / radio hand-offs (search, import and filing, music import lists)
        search.set_content(books.clone(), music.clone());
        activity.set_content(books.clone(), music.clone());
        search.set_radio(radio.clone());
        activity.set_radio(radio.clone());
        integrations.set_music(music.clone());
        // maintenance: backups, logs, update checks, server settings
        let maintenance =
            crate::maintenance::MaintenanceService::new(deps.db.clone(), deps.http.clone(), &deps.config)
                .await?;
        // migration from Radarr/Sonarr/Prowlarr/Readarr/Lidarr/Jellyseerr
        let migrate = {
            std::sync::Arc::new(reelix_migrate::MigrateService::new(
                reelix_migrate::Targets {
                    db: deps.db.clone(),
                    providers: providers.clone(),
                    library: Some(library.clone()),
                    books: Some(books.clone()),
                    music: Some(music.clone()),
                    metadata: library_metadata,
                    // Films and series always have a source: TMDB with a key, open data without one.
                    metadata_ready: std::sync::Arc::new(|| true),
                },
                deps.http.clone(),
            ))
        };
        Ok(Self {
            deps,
            providers,
            library,
            books,
            music,
            radio,
            radio_library,
            requests,
            integrations,
            search,
            activity,
            maintenance,
            migrate,
        })
    }

    pub fn job_handlers(&self, builder: JobSystemBuilder) -> JobSystemBuilder {
        let mut builder = builder;
        // providers
        builder = builder
            .handler(reelix_providers::jobs::IndexerHealthJob(self.providers.clone()))
            .handler(reelix_providers::jobs::ClientHealthJob(self.providers.clone()))
            .long_running_handler(reelix_providers::jobs::DefinitionsSyncJob(self.providers.clone()));
        for t in reelix_providers::jobs::tasks() {
            builder = builder.task(t);
        }
        // library
        builder = reelix_library::jobs::register(builder, &self.library);
        // books / music
        builder = reelix_books::jobs::register(builder, &self.books);
        builder = reelix_music::jobs::register(builder, &self.music);
        // radio (refresh_radio_shows, radio_feed_sync, radio_library_sync)
        builder = reelix_radio::jobs::register(builder, &self.radio);
        // requests
        builder = builder
            .handler(reelix_requests::RequestStatusSyncJob(self.requests.clone()))
            .handler(reelix_requests::WatchlistSyncJob(self.requests.clone()));
        for t in reelix_requests::tasks() {
            builder = builder.task(t);
        }
        // integrations
        builder = crate::integrations::register(builder, &self.integrations);
        // pipeline search (search, rss_sync, process_pending)
        builder = reelix_pipeline::search::jobs::register(builder, &self.search);
        // pipeline activity (download_monitor, process_download, manual_import, prune_finished)
        builder = reelix_pipeline::monitor::jobs::register(builder, &self.activity);
        // maintenance (backup, update_check)
        builder = crate::maintenance::MaintenanceService::register(builder, &self.maintenance);
        builder
    }

    pub fn attach(&self, _queue: &JobQueue, state: AppState) -> AppState {
        let mut state = state;
        // providers
        state = state.with_extension(self.providers.clone());
        // library
        self.library.set_queue(_queue.clone());
        state = state.with_extension(self.library.clone());
        // books / music
        self.books.set_queue(_queue.clone());
        state = state.with_extension(self.books.clone());
        self.music.set_queue(_queue.clone());
        state = state.with_extension(self.music.clone());
        // radio
        self.radio.set_queue(_queue.clone());
        state = state.with_extension(self.radio.clone());
        // requests
        self.requests.set_queue(_queue.clone());
        state = state.with_extension(self.requests.clone());
        // pipeline search
        state = state.with_extension(self.search.clone());
        // pipeline activity
        self.activity.set_queue(_queue.clone());
        state = state.with_extension(self.activity.clone());
        // Health sources: add other services' `HealthSource`s to this list.
        let health: Vec<std::sync::Arc<dyn reelix_api::HealthSource>> = vec![
            self.providers.clone(),
            std::sync::Arc::new(reelix_api::ActivityHealth(self.activity.clone())),
            self.maintenance.clone(),
            self.radio_library.clone(),
        ];
        // integrations (health_watch uses the same sources)
        self.integrations.set_health_sources(health.clone());
        self.integrations.spawn_bridge();
        state = state.with_extension(std::sync::Arc::new(reelix_api::Integrations(
            self.integrations.clone(),
        )));
        state = state.with_extension(std::sync::Arc::new(reelix_api::HealthSources(health)));
        // maintenance
        state = state.with_extension(std::sync::Arc::new(reelix_api::Maintenance(
            self.maintenance.clone(),
        )));
        state = state.with_extension(self.migrate.clone());
        state
    }
}

/// HTTP client shared by outbound integrations (metadata, indexers, clients, notifications).
pub fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("Reelix/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(60))
        .build()?)
}
