# Reelix — design notes

**One self-hosted app for your whole media pipeline: discover, request, search, download, import, and organize movies, TV, books and music.**

Reelix combines six tools into one Rust service with one web UI:

| Replaces | What Reelix does |
|---|---|
| **Radarr** | Monitors movies, searches for releases, grabs them, imports and renames files |
| **Sonarr** | Same for TV: series, seasons, episodes, anime absolute numbering |
| **Prowlarr** | Manages indexers (Torznab/Newznab plus built-in definitions), health checks, rate limits |
| **Jellyseerr** | Discovery and request portal with users, approvals, quotas and media-server login |
| **Readarr** | Monitors authors and books (ebooks and audiobooks), grabs, imports and renames them |
| **Lidarr** | Monitors artists and albums, grabs, imports, tags and renames music |

Running it means **one binary, one database, one config, one login, one API**. You don't have to wire API keys between six apps, keep their state in sync, or run six containers.

> **Status:** Pre-alpha. Movies and TV (M0–M7), books (M8) and music (M9) are implemented, with the gaps listed under [Current Progress](#current-progress). Most of M10 (migration, compatibility, hardening) is done; PostgreSQL support is not. Next up: the 1.0 release (M11). Nothing has been tested against real indexers, download clients or media servers yet.
>
> **Known issue:** in manual testing, an approved movie request was not searched for automatically and did not move from *Requested* to *Available* after downloading. The cause is still under investigation. See [Current Progress](#current-progress).

---

## Table of Contents

1. [Goals & Non-Goals](#goals--non-goals)
2. [Feature Overview](#feature-overview)
3. [Tech Stack](#tech-stack)
4. [Architecture](#architecture)
5. [Repository Layout](#repository-layout)
6. [Domain Model](#domain-model)
7. [Core Pipelines](#core-pipelines)
8. [Download Client Integrations](#download-client-integrations)
9. [Background Jobs](#background-jobs)
10. [API](#api)
11. [Web UI & Screens](#web-ui--screens)
12. [Users, Roles & Permissions](#users-roles--permissions)
13. [Configuration](#configuration)
14. [Deployment](#deployment)
15. [Migrating from the *arr Stack](#migrating-from-the-arr-stack)
16. [Current Progress](#current-progress)
17. [Milestones & Roadmap](#milestones--roadmap)
18. [Testing Strategy](#testing-strategy)
19. [Security](#security)
20. [Performance Targets](#performance-targets)
21. [Observability](#observability)
22. [Development Setup](#development-setup)
23. [Contributing](#contributing)
24. [Open Questions](#open-questions)
25. [License & Legal](#license--legal)

---

## Goals & Non-Goals

### Goals

- **Unified.** One process with one data model. Requests, library, indexers and downloads share state natively, so there's no polling between services.
- **Fast and light.** Idle RAM under 100 MB, a cold start under 1 second, and libraries of 10k+ movies and 2k+ series without slowing the UI.
- **Correct.** Release parsing, quality decisions and file imports are deterministic and heavily tested. Reelix must never delete or overwrite a file the user didn't ask it to.
- **Real-time UI.** Queue progress, activity and notifications are pushed over WebSocket, so the UI never refreshes on a timer.
- **Multi-user from day one.** Admins manage the library. Requesters see only discovery and their own requests.
- **API-first.** Everything the UI does goes through a documented, versioned REST API (OpenAPI spec generated from code).
- **Every media type in one place.** Movies, TV, books (ebooks and audiobooks) and music share the same indexers, download clients, profiles, users and request portal.
- **Easy migration.** You can import existing Sonarr, Radarr, Prowlarr, Jellyseerr, Readarr and Lidarr setups.
- **Compatible.** Expose Sonarr/Radarr/Lidarr/Readarr-compatible API shims (later milestones) so tools like Bazarr, Recyclarr, Overseerr clients, LunaSea/nzb360 and Home Assistant keep working.

### Non-Goals (for 1.0)

- Media **playback or streaming**. That's Jellyfin, Plex or Emby's job, and Reelix integrates with them.
- Reading ebooks or streaming audiobooks and music. Calibre-Web, Kavita, Audiobookshelf, Navidrome and the media servers do that, and Reelix integrates with them.
- Comics and manga (Mylar/Kapowarr territory). The data model shouldn't rule them out.
- A built-in torrent or Usenet client. Reelix talks to existing download clients.
- Subtitle management (Bazarr). Possibly post-1.0.
- Transcoding.

---

## Feature Overview

### Library Management (Radarr + Sonarr)
- Add movies and series by search, TMDB/TVDB/IMDb ID, list import, or request approval.
- Monitoring modes:
  - Movies: monitored / unmonitored, minimum availability (announced, in cinemas, released).
  - Series: all, future, missing, existing, first season, latest season, pilot, none. Monitoring can also be set per season or per episode.
- Series types: **Standard**, **Daily**, **Anime** (absolute numbering, scene/XEM mappings).
- Root folders, a storage overview and free-space checks.
- Library scanning that detects existing files, plus a filesystem watcher for changes.
- Automatic **rename and organize** using tokenized naming templates.
- **Hardlink / atomic move / copy** imports. Hardlinks are the default when source and destination share a filesystem, so seeding can continue.
- Recycle bin with retention instead of hard deletes.
- Calendar of upcoming releases plus an iCal feed.
- Wanted views: missing items, and items that haven't reached their quality cutoff.
- Tags that link items to profiles, indexers, download clients and notifications.

### Quality & Decision Engine
- **Quality definitions** (SDTV through Remux-2160p) with size limits per minute of runtime.
- **Quality profiles** with ordered qualities, grouped qualities, an upgrade cutoff and an "upgrades allowed" toggle.
- **Custom formats**: regex or conditions on title, release group, source, resolution, codec, HDR format, audio, language, edition, indexer flags and size, each with a score.
- Custom format score thresholds: a minimum score and an upgrade-until score.
- **Language profiles** (folded into quality profiles, as in Sonarr v4).
- Delay profiles (prefer Usenet or torrent, wait N minutes for a better release).
- Release restrictions (must contain / must not contain).
- Blocklist covering failed downloads and manual entries.
- **Recyclarr/TRaSH-guide compatible** import of custom formats and profiles (JSON).

### Indexer Management (Prowlarr)
- Native **Torznab** and **Newznab** support.
- Built-in **indexer definitions** in YAML, aiming for compatibility with the Cardigann/Jackett/Prowlarr definition format so the community catalog can be reused.
- Rate limiting, back-off and automatic disable/re-enable on failure.
- Health checks and statistics: queries, grabs, failures and response time per indexer.
- Per-indexer categories, tags, priority and seed ratio/time.
- Proxies: HTTP, SOCKS5 and FlareSolverr for Cloudflare-protected sites.
- A manual **multi-indexer search** page, like Prowlarr's search.
- Optional: expose Reelix as a Torznab/Newznab **aggregator endpoint** for external tools.

### Download Clients
- **Torrent:** qBittorrent, Transmission, Deluge, rTorrent/ruTorrent, a blackhole folder.
- **Usenet:** SABnzbd (first-class, see [SABnzbd spec](#sabnzbd-usenet)), NZBGet, a blackhole folder.
- Per-client category or label, priority, and tags.
- Completed-download handling: automatic import, remove after seeding goals are met.
- Failed-download handling: blocklist the release, then search again automatically.
- Remote path mappings for clients that run on another host or container.

### Requests & Discovery (Jellyseerr)
- A discovery home page with trending, popular, upcoming, by genre, by network or studio, and "because you watched".
- Search across TMDB that shows availability: available, partially available, requested, pending or processing.
- Request a movie, a whole series, or specific seasons. Advanced requests let a user pick a profile or root folder when they have permission.
- Approval workflow with auto-approve per user or role, and manual approve/decline with a reason.
- **Quotas**: N movies or N seasons per X days, per user.
- Login with **Jellyfin, Emby or Plex** accounts as well as local accounts and optional OIDC.
- Users can report issues on available media (audio, video, subtitles, other) and admins resolve them.
- Watchlist sync from Plex/Jellyfin, plus a request-from-watchlist option.
- Request books (ebook and/or audiobook) and albums from the same portal, with their own quotas and auto-approve permissions.
- User notification preferences such as "notify me when my request is available".

### Books (Readarr) — *M8*
- Add authors (monitor all, future or selected books) or single books, by search, ISBN, ASIN or list import.
- Ebooks and audiobooks as separate formats of one book, each with its own quality profile and root folder (for example `epub > azw3 > pdf`, and `m4b > mp3` for audiobooks).
- Editions, series (reading order) and multi-author books.
- Release parsing for book and audiobook releases: author, title, series, format, narrator, bitrate, "unabridged".
- Import with embedded metadata and cover writing (EPUB OPF, M4B tags), multi-file audiobook handling, and an optional Calibre library as the destination.
- Naming templates with author, series and book tokens.
- Metadata from Open Library (default), with Google Books and Hardcover as optional sources.

### Music (Lidarr) — *M9*
- Add artists (monitor all, future, missing or selected albums) or single albums, by search, MusicBrainz ID or list import (Last.fm, Spotify playlists, ListenBrainz).
- Release groups vs. releases (editions), with filters for primary and secondary album types (album, EP, single, live, compilation…).
- Quality profiles for audio: lossy and lossless tiers (MP3 320, AAC, FLAC 16/24-bit), and a preferred-release score.
- Release parsing for music releases: artist, album, year, format, bitrate, source (CD, WEB, vinyl), and "discography" packs.
- Import with track matching against MusicBrainz (track count, duration, tag fingerprints), tag writing, cover art and multi-disc folders.
- Naming templates with artist, album, disc and track tokens.
- Metadata from MusicBrainz and the Cover Art Archive.

### Media Server Integration
- **Jellyfin, Emby, Plex**: trigger a library refresh after import and sync availability.
- Optionally pull watch history to drive recommendations and cleanup rules.
- **Audiobookshelf, Kavita, Calibre-Web, Navidrome**: library refresh after book and music imports (Calibre-Web: best-effort reconnect).

### Notifications
- Discord, Slack, Telegram, Email (SMTP), Pushover, Gotify, ntfy, Apprise, Matrix, generic webhooks and custom scripts.
- Events: grab, import, upgrade, rename, delete, health issue, request created/approved/declined/available, issue reported/resolved, application update.

### Lists & Automation
- Import lists: TMDB lists/collections/popular, Trakt, IMDb lists, Plex/Jellyfin watchlists, MDBList, other Reelix instances.
- Collections (movie franchises) with "monitor whole collection".
- Optional cleanup rules, such as removing watched content after N days (opt-in, with dry-run).

### System
- Health page (indexers failing, clients unreachable, disk space, permissions, update available).
- Task scheduler UI showing run time, next run and manual trigger.
- Event log and activity history.
- Backups: scheduled and manual, as a ZIP of the DB and config, with restore.
- Updates: in-app notice and in-place install with restart for release installs (`install.json` marks the install kind), plus a Docker tag strategy.

---

## Tech Stack

| Layer | Choice | Rationale |
|---|---|---|
| Language | **Rust (stable, edition 2021+)** | Performance, memory safety, single static binary |
| Async runtime | **tokio** | Ecosystem standard |
| HTTP server | **axum** | Tower middleware, ergonomic extractors, WebSocket support |
| HTTP client | **reqwest** (rustls) | Indexers, metadata, download clients |
| Database | **SQLite** (default) via **sqlx**; **PostgreSQL** optional | Zero-config for home users, Postgres for large or shared setups |
| Migrations | `sqlx migrate` (embedded in binary) | Migrations run at startup |
| Serialization | **serde**, serde_json, quick-xml (Torznab/Newznab RSS) | |
| Frontend | **Leptos** (Rust → WASM, client-side rendered, bundled with **Trunk**) and served by axum as static files | Whole stack in Rust with shared types between API and UI; CSR keeps the UI a pure API client |
| Styling | Hand-written CSS with design tokens (CSS custom properties) | No Node toolchain; dark mode first, light theme override |
| API docs | **utoipa** → OpenAPI 3.1 + Scalar/Swagger UI at `/api/docs` | Generated from handler types |
| Auth | argon2id (passwords), DB-backed session cookies (hashed tokens), hashed API keys, `openidconnect` (OIDC, M6) | |
| Scheduling | Internal scheduler on tokio + a persistent job table | Survives restarts; visible in UI |
| Release parsing | Custom parser crate (`regex` + `nom` where needed) | Core IP; needs exhaustive tests |
| Media info | `ffprobe` (bundled in Docker image) | Codec, resolution, HDR, audio tracks, runtime |
| Audio tags *(M8/M9)* | **lofty** | Read and write ID3, Vorbis, FLAC, MP4/M4B tags for music and audiobooks |
| Ebook metadata *(M8)* | EPUB OPF via `zip` + quick-xml | Read and write title, author, series, ISBN, cover |
| Book / music metadata *(M8/M9)* | Open Library (+ optional Google Books, Hardcover); MusicBrainz + Cover Art Archive | Free, open sources with stable IDs; MusicBrainz is rate-limited to 1 req/s, so results are cached aggressively |
| FS watching | **notify** | Detect manual changes in library |
| Templating (naming) | Custom token engine | Sonarr/Radarr-compatible tokens |
| Logging/tracing | **tracing**, tracing-subscriber, optional OpenTelemetry export | |
| Metrics | Prometheus endpoint (`metrics` + `metrics-exporter-prometheus`) | |
| Config | **figment** (TOML file + env vars + CLI flags) | |
| Error handling | `thiserror` (libraries), `anyhow` (binary edges) | |

> **Frontend alternative:** If Leptos becomes a bottleneck (component ecosystem, dev velocity), the fallback is a TypeScript SPA (SvelteKit or React + Vite) served by axum as static files. The API-first design keeps this swap cheap. **Decision: start with Leptos (CSR) and revisit at the end of M1.** SSR + hydration was considered and deferred: CSR avoids coupling the server binary to the UI build, and the server already injects `<base href>` for `url_base` and a hash-based CSP for the Trunk loader script.

---

## Architecture

### High-Level

```
                         ┌──────────────────────────────────────────────┐
  Browser (Leptos UI) ───┤  axum HTTP server                            │
  3rd-party tools ───────┤  ├─ /api/v1/*      REST (OpenAPI)            │
  Media servers (hooks) ─┤  ├─ /api/v1/ws     WebSocket (events)        │
                         │  ├─ /torznab/*     aggregator (optional)     │
                         │  ├─ /api/v3/*      Sonarr/Radarr shim (later)│
                         │  └─ /*             SSR UI + static assets    │
                         └───────────────┬──────────────────────────────┘
                                         │
                ┌────────────────────────┼─────────────────────────┐
                │                 Application Services             │
                │  ┌──────────┐ ┌───────────┐ ┌─────────────────┐  │
                │  │ Library  │ │ Requests  │ │ Users / Auth    │  │
                │  └────┬─────┘ └─────┬─────┘ └─────────────────┘  │
                │       │             │                            │
                │  ┌────▼─────────────▼──┐   ┌──────────────────┐  │
                │  │  Search & Decision  │◄──┤ Release Parser   │  │
                │  │  Engine             │   └──────────────────┘  │
                │  └────┬───────────┬────┘                         │
                │       │           │                              │
                │  ┌────▼────┐ ┌────▼──────┐ ┌──────────────────┐  │
                │  │Indexers │ │ Download  │ │ Import / Rename  │  │
                │  │ Manager │ │ Clients   │ │ / FS ops         │  │
                │  └────┬────┘ └────┬──────┘ └───────┬──────────┘  │
                │       │           │                │             │
                │  ┌────▼───────────▼────────────────▼──────────┐  │
                │  │        Event Bus (tokio broadcast)         │  │
                │  └────┬──────────────┬───────────────┬────────┘  │
                │  ┌────▼─────┐ ┌──────▼──────┐ ┌──────▼───────┐   │
                │  │Notifier  │ │ Media Server│ │ WS Broadcast │   │
                │  └──────────┘ │ Sync        │ └──────────────┘   │
                │               └─────────────┘                    │
                │  ┌────────────────────────────────────────────┐  │
                │  │ Scheduler + Persistent Job Queue           │  │
                │  └────────────────────────────────────────────┘  │
                └──────────────────────┬───────────────────────────┘
                                       │
           ┌───────────────┬───────────┴───┬──────────────────┬───────────────┐
           ▼               ▼               ▼                  ▼               ▼
      SQLite/Postgres  Filesystem    Metadata APIs       Indexers        Download clients
                       (media,       (TMDB, TVDB,        (Torznab/       (qBit, SAB, …)
                        downloads)    XEM, Trakt,         Newznab, YAML)
                                      MusicBrainz,
                                      Open Library)
```

### Architectural Principles

1. **Modular monolith.** One deployable binary, split internally into crates with clear boundaries. Each domain crate exposes a service trait, and the server crate wires them together.
2. **Hexagonal / ports & adapters.** Every external system (indexer, download client, metadata provider, media server, notifier) is a **trait** with multiple adapters. Adding a new download client means adding one adapter file plus a settings schema.
3. **Event-driven side effects.** Core operations emit domain events (`MovieGrabbed`, `EpisodeImported`, `RequestApproved`, …). Notifications, media-server refreshes, WebSocket pushes and history logging all subscribe to events instead of being called directly.
4. **Persistent jobs.** Long or retryable work (searches, imports, refreshes) goes through a DB-backed job queue with retries, back-off and deduplication, so a crash mid-import resumes safely.
5. **Dynamic provider settings.** Each adapter declares its settings schema (fields, types, validation, help text). The UI renders the forms generically, so new providers need no frontend work.
6. **Shared types.** Request/response DTOs live in `reelix-api-types`, which the server and the Leptos frontend both compile. API and UI can't drift.

### Key Traits (sketch)

```rust
#[async_trait]
pub trait Indexer: Send + Sync {
    fn capabilities(&self) -> &IndexerCapabilities;
    async fn search(&self, query: &SearchQuery) -> Result<Vec<ReleaseInfo>, IndexerError>;
    async fn rss(&self) -> Result<Vec<ReleaseInfo>, IndexerError>;
    async fn test(&self) -> Result<(), IndexerError>;
}

#[async_trait]
pub trait DownloadClient: Send + Sync {
    fn protocol(&self) -> Protocol; // Torrent | Usenet
    async fn add(&self, release: &RemoteRelease, opts: &AddOptions) -> Result<DownloadId, ClientError>;
    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError>;
    async fn remove(&self, id: &DownloadId, delete_data: bool) -> Result<(), ClientError>;
    async fn test(&self) -> Result<(), ClientError>;
}

#[async_trait]
pub trait MetadataProvider: Send + Sync {
    async fn movie(&self, id: TmdbId) -> Result<MovieMetadata, MetaError>;
    async fn series(&self, id: SeriesExternalId) -> Result<SeriesMetadata, MetaError>;
    async fn search(&self, q: &str, kind: MediaKind) -> Result<Vec<SearchHit>, MetaError>;
    async fn discover(&self, feed: DiscoverFeed, page: u32) -> Result<Page<SearchHit>, MetaError>;
}

#[async_trait]
pub trait Notifier: Send + Sync {
    fn supported_events(&self) -> EventMask;
    async fn notify(&self, event: &DomainEvent) -> Result<(), NotifyError>;
}

#[async_trait]
pub trait MediaServer: Send + Sync {
    async fn authenticate(&self, creds: &Credentials) -> Result<ExternalUser, MsError>;
    async fn refresh(&self, path: &Path) -> Result<(), MsError>;
    async fn library_items(&self) -> Result<Vec<MsItem>, MsError>;
}
```

---

## Repository Layout

```
reelix/
├── Cargo.toml                  # workspace
├── crates/
│   ├── reelix-server/          # bin: config, wiring, axum router, startup/shutdown
│   ├── reelix-core/            # domain entities, value objects, events, error types
│   ├── reelix-db/              # sqlx repositories, migrations (sqlite + postgres)
│   ├── reelix-api/             # axum handlers, auth middleware, OpenAPI, WS hub
│   ├── reelix-api-types/       # DTOs shared by server + frontend (no tokio deps)
│   ├── reelix-parser/          # release title parser (quality, S/E, group, langs…)
│   ├── reelix-decision/        # quality profiles, custom formats, scoring, specs
│   ├── reelix-metadata/        # TMDB, TVDB, XEM, Trakt clients + caching, Open Library, MusicBrainz + Cover Art Archive
│   ├── reelix-indexers/        # Torznab, Newznab, YAML definition engine, ratelimit
│   ├── reelix-downloaders/     # qBittorrent, Transmission, Deluge, SAB, NZBGet…
│   ├── reelix-import/          # ffprobe, file matching, rename engine, FS ops
│   ├── reelix-library/         # movie/series services, monitoring, refresh, scan
│   ├── reelix-books/           # authors, books, editions, ebook/audiobook import
│   ├── reelix-music/           # artists, albums, tracks, tagging
│   ├── reelix-requests/        # requests, approvals, quotas, issues, discovery
│   ├── reelix-mediaserver/     # Jellyfin, Emby, Plex
│   ├── reelix-notify/          # notifier adapters
│   ├── reelix-jobs/            # scheduler, persistent queue, workers
│   ├── reelix-migrate/         # importers for Sonarr/Radarr/Prowlarr/Jellyseerr (+ Readarr/Lidarr); placeholder today
│   └── reelix-web/             # Leptos frontend (components, pages, routing)
├── definitions/                # indexer YAML definitions (git submodule or synced)
├── migrations/
│   ├── sqlite/
│   └── postgres/
├── tests/
│   ├── fixtures/release-names/ # thousands of real-world titles + expected parse
│   ├── fixtures/indexers/      # recorded Torznab/Newznab XML responses
│   └── e2e/                    # Playwright tests against a running instance
├── docker/
│   ├── Dockerfile
│   └── docker-compose.example.yml
├── docs/                       # mdBook: user guide, API guide, provider guide
└── xtask/                      # cargo xtask: release, gen-openapi, sync-definitions
```

---

## Domain Model

Core tables (simplified). All tables have `id`, `created_at`, `updated_at`.

### Library

| Entity | Key fields |
|---|---|
| `movies` | tmdb_id, imdb_id, title, sort_title, year, status, overview, runtime, studio, genres, certification, images, monitored, minimum_availability, quality_profile_id, root_folder_id, path, collection_id, added_by_request_id, tags |
| `movie_files` | movie_id, relative_path, size, quality, custom_formats, media_info (json), languages, release_group, edition, date_added, original_path |
| `collections` | tmdb_id, title, monitored, quality_profile_id, root_folder_id |
| `series` | tmdb_id, tvdb_id, imdb_id, title, sort_title, year, status (continuing/ended/upcoming), network, series_type (standard/daily/anime), monitored, monitor_new_items, season_folder, quality_profile_id, root_folder_id, path, tags |
| `seasons` | series_id, season_number, monitored |
| `episodes` | series_id, season_number, episode_number, absolute_number, scene_season/episode/absolute, title, air_date_utc, monitored, episode_file_id |
| `episode_files` | series_id, season_number, relative_path, size, quality, custom_formats, media_info, languages, release_group |
| `root_folders` | path, media_kind, free_space_cache |
| `tags` | label |

### Books *(M8)*

| Entity | Key fields |
|---|---|
| `authors` | openlibrary_id, name, sort_name, overview, images, monitored, monitor_new_items, quality_profile_id (ebook), audio_quality_profile_id, root_folder_id, path, tags |
| `books` | author_id, openlibrary_id, isbn13, asin, title, series_id, series_position, release_date, monitored_ebook, monitored_audiobook, added_by_request_id |
| `book_series` | title, openlibrary_id |
| `book_files` | book_id, format (ebook/audiobook), relative_path(s), size, quality (epub/azw3/pdf/m4b/mp3…), bitrate, narrator, release_group |

### Music *(M9)*

| Entity | Key fields |
|---|---|
| `artists` | musicbrainz_id, name, sort_name, disambiguation, type, images, monitored, monitor_new_items, quality_profile_id, metadata_profile_id (album types), root_folder_id, path, tags |
| `albums` | artist_id, musicbrainz_release_group_id, selected_release_id, title, album_type, secondary_types[], release_date, monitored, added_by_request_id |
| `tracks` | album_id, musicbrainz_recording_id, disc_number, track_number, title, duration_ms, track_file_id |
| `track_files` | album_id, relative_path, size, quality (FLAC 24/16, MP3 320/V0, AAC…), bitrate, sample_rate, tags (json) |

`root_folders.media_kind`, `requests.media_kind`, `history` and `queue_items` gain `book` and `music` variants rather than separate tables.

### Decision

| Entity | Key fields |
|---|---|
| `quality_definitions` | quality, title, min/max/preferred size per minute |
| `quality_profiles` | name, media_kind, items (ordered json), cutoff, upgrade_allowed, min_format_score, cutoff_format_score, format_items (custom_format_id → score), language |
| `custom_formats` | name, include_in_rename, specifications (json: type, negate, required, value) |
| `delay_profiles` | preferred_protocol, usenet_delay, torrent_delay, bypass_if_highest_quality, tags, order |
| `release_profiles` | required[], ignored[], indexer_id, tags |
| `blocklist` | media ref, source_title, quality, protocol, indexer, message, date |

### Indexers & Downloads

| Entity | Key fields |
|---|---|
| `indexers` | name, implementation (torznab/newznab/definition:<id>), settings (json, secrets encrypted), protocol, priority, enable_rss, enable_auto_search, enable_interactive_search, tags, proxy_id |
| `indexer_status` | indexer_id, failure_count, disabled_until, last_failure_reason |
| `indexer_stats` | indexer_id, date, queries, grabs, failures, avg_response_ms |
| `proxies` | kind (http/socks5/flaresolverr), host, port, credentials |
| `download_clients` | name, implementation, settings, protocol, priority, enabled, remove_completed, remove_failed, tags |
| `remote_path_mappings` | host, remote_path, local_path |
| `queue_items` (tracked downloads) | download_client_id, download_id, media ref(s), release snapshot (json), state, status_messages, output_path |
| `history` | media ref, event_type (grabbed/imported/failed/deleted/renamed/ignored), source_title, quality, data (json), date |

### Users & Requests

| Entity | Key fields |
|---|---|
| `users` | username, email, display_name, avatar, password_hash (nullable), auth_source (local/jellyfin/emby/plex/oidc), external_id, permissions (bitflags), movie_quota_limit/days, tv_quota_limit/days, locale |
| `api_keys` | user_id, name, hashed_key, last_used, scopes |
| `sessions` | managed by tower-sessions store |
| `requests` | user_id, media_kind, tmdb_id, seasons[] (tv), status (pending/approved/declined/processing/available/failed), is_4k, profile/root overrides, modified_by, decline_reason, linked movie_id/series_id |
| `issues` | user_id, media ref, type, message, status, comments[] |
| `user_notification_settings` | user_id, channel, event_mask, target |
| `watchlist_sync` | user_id, source, last_synced |

### System

| Entity | Key fields |
|---|---|
| `jobs` | kind, payload (json), status, priority, attempts, run_after, dedupe_key, started_at, finished_at, error |
| `scheduled_tasks` | kind, interval, last_run, next_run, last_duration |
| `notifications` | implementation, settings, events (mask), tags |
| `media_servers` | implementation, settings, sync options |
| `import_lists` | implementation, settings, target profile/root, monitor option, auto-add, interval |
| `settings` | key, value (json). UI/naming/media-management/general settings |
| `metadata_cache` | provider, key, payload, expires_at |

Secrets (API keys, passwords) in provider settings are **encrypted at rest** with a key derived from `REELIX_SECRET_KEY` (AES-256-GCM).

---

## Core Pipelines

### 1. Release Parsing (`reelix-parser`)

Input: `Show.Name.S02E05.1080p.WEB-DL.DDP5.1.Atmos.H.264-GROUP`

Output:

```rust
ParsedRelease {
    title: "Show Name",
    year: None,
    kind: Episode { season: 2, episodes: [5], absolute: [], air_date: None, full_season: false },
    quality: Quality { source: WebDl, resolution: R1080p, modifier: None },
    revision: Revision { version: 1, real: 0, proper: false, repack: false },
    languages: [English],
    release_group: Some("GROUP"),
    edition: None,
    hdr: [], video_codec: Some(H264), audio: [DDP 5.1, Atmos],
    hash: None,
}
```

Requirements:
- Handles movies, standard TV, multi-episode (S01E01E02, S01E01-E03), full seasons, multi-season packs, daily (2024.05.01), anime (`[Group] Show - 1052 [1080p]`), specials, and foreign titles and languages.
- Detects PROPER/REPACK/REAL/v2, editions (Director's Cut, Extended, IMAX…), HDR (DV, HDR10, HDR10+, HLG), 3D, remux, BR-DISK.
- A **fixture-driven test suite** of 5,000+ real titles with expected output is a blocking CI gate.
- Pure and synchronous, with no I/O. It's fuzz-tested (`cargo fuzz`) so it never panics.

### 2. Search → Grab

```
Trigger (RSS sync | missing search | manual | new request approved | upgrade)
   │
   ▼
Build SearchCriteria (titles + aliases, year, IDs, seasons/eps, scene mappings)
   │
   ▼
Indexer Manager ── fan-out (concurrent, per-indexer rate limits) ──► Torznab/Newznab/YAML
   │   collects ReleaseInfo[], records stats/failures
   ▼
Parse each release → map to Movie / Episodes (by IDs first, then title matching)
   │
   ▼
Decision Engine: run Specifications
   ├─ Not blocklisted            ├─ Quality allowed by profile
   ├─ Size within limits         ├─ Custom format score ≥ min
   ├─ Is upgrade over existing   ├─ Release/restriction rules
   ├─ Not already in queue       ├─ Minimum availability / air date
   ├─ Seeders / age / retention  ├─ Delay profile
   └─ Language matches           └─ Indexer & tag constraints
   │  → Accepted / Rejected(reasons[]) (reasons shown in interactive search)
   ▼
Prioritize accepted: quality ▸ CF score ▸ protocol pref ▸ indexer priority ▸ seeders/age ▸ size
   │
   ▼
Send to DownloadClient (chosen by protocol, tags, priority) → create queue_item
   │
   ▼
Emit `ReleaseGrabbed` → history, notifications, WS push, request status = Processing
```

### 3. Download Tracking → Import

```
Every 30–60s: poll each download client → reconcile with queue_items
   │
   ├─ downloading → update progress (WS push)
   ├─ failed      → blocklist + emit DownloadFailed + (optional) re-search
   └─ completed   →
         │
         ▼
   Resolve local path (remote path mappings)
   │
   ▼
   Find media files (ignore samples, extras rules), ffprobe each
   │
   ▼
   Match files → episodes/movie (parse filename + folder + tracked release)
   │
   ▼
   Import Specifications: is upgrade, free space, not sample, matches expected, unpack done…
   │
   ▼
   Execute: hardlink (default if same FS) | atomic move | copy  →  rename via template
   │       old file → recycle bin (if upgrade)
   ▼
   DB update (files, history) → Emit `MediaImported` / `MediaUpgraded`
   │
   ▼
   Subscribers: media-server refresh, notifications, request → Available, WS push
   │
   ▼
   Post-import: client-side removal once seed goals met (if enabled)
```

If an import can't be matched automatically (ambiguous or unknown), the item lands in **Manual Import** with the reasons attached.

### 4. Request Lifecycle

```
User requests media
   │
   ├─ Quota check ──► exceeded → reject with message
   ▼
status = Pending ── auto-approve permission? ──yes──┐
   │ no                                             │
   ▼                                                ▼
Admin approves/declines (with reason)         status = Approved
                                                    │
                                                    ▼
                         Create/Update Movie or Series (monitor requested seasons)
                         using default or overridden profile + root folder
                                                    │
                                                    ▼
                                    Trigger search job → status = Processing
                                                    │
                                                    ▼
                    MediaImported for all requested items → status = Available
                                                    │
                                                    ▼
                                    Notify requester (per-user preferences)
```

Because the library and requests share a database, **availability is always exact**. There's no "sync with Radarr" lag.

### 5. Naming Engine

Sonarr/Radarr-compatible tokens so users can paste existing formats:

```
Movie folder:   {Movie CleanTitle} ({Release Year}) {tmdb-{TmdbId}}
Movie file:     {Movie CleanTitle} ({Release Year}) {edition-{Edition Tags}} [{Custom Formats}][{Quality Full}][{MediaInfo VideoDynamicRangeType}]{-Release Group}
Series folder:  {Series TitleYear} {tvdb-{TvdbId}}
Season folder:  Season {season:00}
Episode file:   {Series TitleYear} - S{season:00}E{episode:00} - {Episode CleanTitle} [{Quality Full}]{-Release Group}
Anime file:     {Series TitleYear} - S{season:00}E{episode:00} - {absolute:000} - {Episode CleanTitle} [{Quality Full}]{-Release Group}
```

Includes a live preview in settings, illegal-character replacement, colon handling options, and a max path length guard.

Books and music get their own Readarr/Lidarr-compatible tokens:

```
Author folder:  {Author Name}
Book file:      {Author Name} - {Book Series}{ #{Book SeriesPosition}} - {Book Title} ({Release Year})
Artist folder:  {Artist Name}
Album folder:   {Album Title} ({Release Year}) [{Quality Title}]
Track file:     {medium:0}{track:00} - {Track Title}
```

### 6. Books & Music *(M8/M9)*

Books and music reuse the same pipelines (search → decision → grab → track → import) with media-specific pieces plugged in:

- **Parsing:** separate parser modules for book/audiobook and music release names, with their own fixture suites. TV and movie parsing is untouched.
- **Matching:** books match by ISBN/ASIN first, then author + title. Albums match by MusicBrainz release (track count and durations), with a manual "choose release" step when the match is ambiguous.
- **Decision:** quality profiles gain `media_kind = book | audiobook | music` with their own quality lists. Custom formats and delay profiles work unchanged.
- **Import:** multi-file units (an album or a multi-part audiobook) import atomically: all files or none. Tags and covers are written after import, never to files still seeding.

---

## Download Client Integrations

Every client is an adapter implementing the `DownloadClient` trait in `reelix-downloaders`, and each one declares its own settings schema. This section is the detailed spec for each adapter.

### SABnzbd (Usenet)

**Crate module:** `reelix-downloaders/src/sabnzbd/` (`mod.rs`, `api.rs`, `models.rs`, `health.rs`)
**Protocol:** Usenet · **Supported versions:** SABnzbd **3.0+** (tested against the latest 3.x and 4.x) · **Milestone:** M4, as the first Usenet client to ship.

#### Settings Schema

| Field | Type | Default | Notes |
|---|---|---|---|
| `name` | string | `SABnzbd` | Display name. Multiple instances are allowed. |
| `host` | string | `localhost` | Hostname, IP or container name |
| `port` | u16 | `8080` | |
| `use_ssl` | bool | `false` | `verify_ssl` toggle shown when on |
| `url_base` | string | `/sabnzbd` | Matches SABnzbd's `url_base` setting |
| `api_key` | secret | — | Must be the **full API key**. The NZB key can only add jobs, so it's rejected. |
| `username` / `password` | secret | — | Only if SABnzbd sits behind HTTP basic auth |
| `movie_category` | string | `movies` | Picked from a dropdown filled by `mode=get_cats` |
| `tv_category` | string | `tv` | |
| `recent_priority` | enum | `Default` | Applies to releases aired or released in the last 14 days: Default / Paused / Low / Normal / High / Force |
| `older_priority` | enum | `Default` | Applies to everything else |
| `grab_method` | enum | `upload_nzb` | `upload_nzb` (Reelix fetches the NZB and uploads it) or `send_url` |
| `priority` | u8 | `1` | Client priority when several Usenet clients match |
| `remove_completed` / `remove_failed` | bool | `true` / `true` | Remove from SABnzbd history after import or failure handling |
| `tags` | tag[] | — | Limits this client to tagged media |

The API key and password are encrypted at rest like every other provider secret.

#### API Mapping

All calls go to `GET|POST {scheme}://{host}:{port}{url_base}/api?apikey=…&output=json&mode=…`.

| Reelix operation | SABnzbd call | Notes |
|---|---|---|
| `test()` | `mode=version`, then `mode=get_config` and `mode=get_cats` | Checks connectivity, the minimum version, a valid full API key and that the configured categories exist |
| `add()`, upload | `POST mode=addfile` (multipart `name` = NZB bytes) with `cat`, `priority`, `nzbname` | Default. The indexer's API key never reaches SABnzbd, and Reelix can validate the NZB (non-empty, parseable XML) before sending it. |
| `add()`, URL | `mode=addurl&name=<nzb url>` with `cat`, `priority`, `nzbname` | Fallback for indexers whose NZB links SABnzbd must fetch itself |
| `items()` | `mode=queue&cat=<category>` + `mode=history&category=<category>&limit=<n>` | Merged into one list. **Only Reelix's categories are read**, so other downloads in SABnzbd are never touched. |
| `remove()` | `mode=queue&name=delete&value=<nzo_id>&del_files=<0/1>` or `mode=history&name=delete&value=<nzo_id>&del_files=<0/1>&archive=0` | Queue vs. history is decided by where the item currently is |
| Change priority | `mode=queue&name=priority&value=<nzo_id>&value2=<prio>` | Exposed in the Queue screen |
| Health and warnings | `mode=get_config&section=misc`, `mode=warnings` | Feeds the health checks below |

The SABnzbd `nzo_id` returned by `add()` (for example `SABnzbd_nzo_abc123`) is stored as `queue_items.download_id`. Priority values map to SABnzbd as Default `-100`, Paused `-2`, Low `-1`, Normal `0`, High `1`, Force `2`.

#### Status Mapping

| SABnzbd state | Source | Reelix `DownloadState` |
|---|---|---|
| `Queued`, `Grabbing`, `Fetching`, `Propagating` | queue | `Queued` |
| `Downloading` | queue | `Downloading` (progress = `1 - mbleft/mb`, ETA from `timeleft`) |
| `Paused`, or global queue paused | queue | `Paused` |
| `Checking`, `QuickCheck`, `Verifying`, `Repairing`, `Extracting`, `Moving`, `Running` | history | `PostProcessing`. Reelix never imports before SABnzbd finishes. |
| `Completed` | history | `Completed`. Output path = history `storage` field, then remote path mapping. |
| `Failed` | history | `Failed` with `fail_message` attached |

Failure reasons are classified from `fail_message` so the right action is taken:

| Failure | Action |
|---|---|
| Missing articles, repair failed, unpacking failed | Blocklist the release, then search again if enabled |
| Encrypted or password-protected archive | Blocklist with reason "password protected", then search again |
| Out of disk space, or the job was aborted for being too large | **No blocklist** (not the release's fault); raise a health warning |
| NZB fetch failed / duplicate rejected by SABnzbd | Mark the grab as failed, blocklist, then search again |

#### Import Behaviour

- Once the history status is `Completed`, the `storage` path is resolved through **remote path mappings** and handed to the normal import pipeline (hardlink/move/copy + rename).
- Usenet downloads have no seeding, so after a successful import Reelix moves the files by default and removes the history entry when `remove_completed` is on.
- If `storage` doesn't exist locally, the item goes to **Manual Import** with a "path not found" warning instead of failing silently.

#### Health Checks

Each check is shown on **System → Health** with a link to the relevant SABnzbd setting:

- SABnzbd is unreachable, or the API key is invalid or only an NZB key.
- The version is below the supported minimum.
- A configured category doesn't exist in SABnzbd.
- **Sorting is enabled** for a Reelix category (TV, movie or date sorting in SABnzbd). Sorting renames files before Reelix can match them.
- History retention is set to delete immediately, so Reelix can't see completed jobs.
- A category's completed folder isn't reachable locally and no remote path mapping covers it.
- SABnzbd reports a server or disk warning through `mode=warnings`.

#### UI

- **Settings → Download Clients → Add → SABnzbd:** the schema-driven form, a **Test** button that reports each check separately (connect, auth, version, categories), and category dropdowns filled live from SABnzbd.
- **Activity → Queue:** shows SABnzbd items with progress, ETA, post-processing stage (for example "Repairing" or "Extracting"), and actions to change priority, remove, or remove and blocklist.

#### Adapter Sketch

```rust
pub struct Sabnzbd {
    http: reqwest::Client,
    settings: SabnzbdSettings,
}

#[async_trait]
impl DownloadClient for Sabnzbd {
    fn protocol(&self) -> Protocol { Protocol::Usenet }

    async fn add(&self, release: &RemoteRelease, opts: &AddOptions) -> Result<DownloadId, ClientError> {
        let category = self.settings.category_for(opts.media_kind);
        let priority = self.settings.priority_for(opts.is_recent);
        let resp: AddResponse = match self.settings.grab_method {
            GrabMethod::UploadNzb => self.add_file(&release.nzb_bytes().await?, &release.title, category, priority).await?,
            GrabMethod::SendUrl   => self.add_url(&release.download_url, &release.title, category, priority).await?,
        };
        resp.nzo_ids.into_iter().next()
            .map(DownloadId::from)
            .ok_or(ClientError::Rejected("SABnzbd returned no nzo_id".into()))
    }

    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError> {
        let (queue, history) = tokio::try_join!(self.queue(), self.history())?;
        Ok(queue.into_iter().map(Into::into).chain(history.into_iter().map(Into::into)).collect())
    }

    async fn remove(&self, id: &DownloadId, delete_data: bool) -> Result<(), ClientError> { /* queue or history delete */ }
    async fn test(&self) -> Result<(), ClientError> { /* version → config → categories */ }
}
```

#### Tests

- `wiremock` fixtures recorded from real SABnzbd 3.x and 4.x responses covering version, get_config, get_cats, addfile, addurl, queue, history and the failure messages above.
- Unit tests for the status mapping and failure classification tables.
- An end-to-end test (compose stack) that grabs from the mock Newznab indexer through a real SABnzbd container to an imported, renamed file.

---

## Background Jobs

| Task | Default interval | Notes |
|---|---|---|
| RSS Sync | 15 min | Pull RSS from all RSS-enabled indexers; evaluate against wanted items |
| Download Queue Poll | 30 s (when items active), 2 min idle | Track progress, completion, failures |
| Refresh Movie Metadata | 24 h | Staggered across library |
| Refresh Series Metadata | 12 h | New episodes, air date changes |
| Refresh Author / Artist Metadata *(M8/M9)* | 24 h | New books and albums; throttled to MusicBrainz's 1 req/s |
| Scene Mapping Sync (XEM) | 6 h | Anime / scene numbering |
| Import Lists Sync | 6 h (per list configurable) | |
| Watchlist Sync | 1 h | Plex/Jellyfin watchlists → requests |
| Missing / Cutoff Search | Off by default (manual) | Throttled, respects indexer limits |
| Media Server Availability Sync | 6 h + after each import | |
| Indexer Health Check | 15 min | Re-enable after back-off |
| Disk Space Check | 15 min | Health warning thresholds |
| Housekeeping | 24 h | Prune history/logs, vacuum DB, clean metadata cache, orphaned files |
| Backup | 7 d | Keep last N |
| Recycle Bin Cleanup | 24 h | Retention days |
| Update Check | 24 h | |

The job runner is a fixed worker pool with **per-kind concurrency limits** (for example, at most one RSS sync and at most two imports at once). Jobs are deduplicated by `dedupe_key`.

---

## API

- **Base:** `/api/v1`
- **Auth:** session cookie (UI), `X-Api-Key` header or `?apikey=` (integrations), Bearer token (OIDC, optional)
- **Docs:** `/api/docs` (interactive), `/api/v1/openapi.json`
- **Real-time:** `/api/v1/ws` sends JSON event frames: `{ "type": "queue.updated", "data": {...} }`

### Endpoint Groups

| Group | Examples |
|---|---|
| Auth | `POST /auth/login`, `POST /auth/logout`, `POST /auth/jellyfin`, `POST /auth/plex`, `GET /auth/oidc/callback`, `GET /auth/me` |
| Movies | `GET/POST /movies`, `GET/PUT/DELETE /movies/{id}`, `POST /movies/lookup?term=`, `POST /movies/{id}/search`, `GET /movies/{id}/releases` |
| Series | `GET/POST /series`, `GET/PUT/DELETE /series/{id}`, `GET /series/{id}/episodes`, `PUT /episodes/monitor`, `POST /series/{id}/search` |
| Books *(M8)* | `GET/POST /authors`, `GET/PUT/DELETE /authors/{id}`, `GET/POST /books`, `POST /books/{id}/search`, `GET /bookfiles` |
| Music *(M9)* | `GET/POST /artists`, `GET/PUT/DELETE /artists/{id}`, `GET /albums`, `PUT /albums/{id}/release`, `POST /albums/{id}/search`, `GET /trackfiles` |
| Files | `GET /moviefiles`, `GET /episodefiles`, `DELETE …`, `GET /rename/preview`, `POST /rename` |
| Manual Import | `GET /manualimport?folder=`, `POST /manualimport` |
| Releases | `GET /releases?movieId=|episodeId=` (interactive search), `POST /releases/grab` |
| Indexers | CRUD `/indexers`, `GET /indexers/schema`, `POST /indexers/{id}/test`, `GET /indexers/stats`, `GET /indexers/search?q=&cat=` |
| Download Clients | CRUD `/downloadclients`, `/downloadclients/schema`, `/test` |
| Queue | `GET /queue`, `DELETE /queue/{id}?blocklist=&removeFromClient=`, `POST /queue/{id}/import` |
| History / Blocklist | `GET /history`, `GET/DELETE /blocklist` |
| Calendar / Wanted | `GET /calendar?start=&end=`, `GET /calendar.ics?apikey=`, `GET /wanted/missing`, `GET /wanted/cutoff` |
| Profiles | CRUD `/qualityprofiles`, `/customformats`, `/delayprofiles`, `/releaseprofiles`, `/qualitydefinitions` |
| Discover | `GET /discover/{feed}`, `GET /search?q=` (TMDB + availability), `GET /media/{kind}/{tmdbId}` |
| Requests | `GET/POST /requests`, `POST /requests/{id}/approve`, `POST /requests/{id}/decline`, `GET /requests/count` |
| Issues | CRUD `/issues`, `POST /issues/{id}/comments` |
| Users | CRUD `/users`, `POST /users/import/{jellyfin|plex}`, `GET /users/{id}/quota` |
| Notifications / Media Servers / Import Lists | CRUD + `/schema` + `/test` |
| System | `GET /system/status`, `GET /system/health`, `GET /system/health/live`, `GET/POST /system/tasks`, `GET /system/logs`, `POST /system/backup`, `POST /system/restore` |
| Settings | `GET/PUT /settings/{section}` (general, media-management, naming, ui, requests, security) |

### Compatibility Shims (M10)

- `/api/v3/*` Radarr- and Sonarr-shaped subsets (separate namespaces via `/radarr/api/v3` and `/sonarr/api/v3`) for Bazarr, Recyclarr, Homepage, LunaSea/nzb360 and so on.
- `/torznab/{indexerId}/api` exposes Reelix's indexers to external tools.
- `/readarr/api/v1` and `/lidarr/api/v1` subsets for book and music tools.

---

## Web UI & Screens

Design language: **dark-first**, responsive (desktop, tablet and phone), poster-grid and table views, keyboard shortcuts (`/` search, `g m` movies, `g s` series, `g q` queue), and real-time updates over WebSocket. The UI is split into an **Admin/Library** area and a **Request** area. Requesters see only the Request area.

### Navigation Map

```
┌ Discover (home for requesters)
├ Requests
├ Issues
│
├ Movies ────────┬ Library
│                ├ Add New
│                ├ Collections
│                └ Import Library
├ Series ────────┬ Library
│                ├ Add New
│                └ Import Library
├ Books ─────────┬ Authors · Add New · Import Library        (M8)
├ Music ─────────┬ Artists · Add New · Import Library        (M9)
├ Calendar
├ Activity ──────┬ Queue
│                ├ History
│                └ Blocklist
├ Wanted ────────┬ Missing
│                └ Cutoff Unmet
├ Indexers ──────┬ Indexers
│                ├ Search
│                └ Stats
├ Settings ──────┬ General · UI · Media Management · Naming · Profiles · Quality
│                ├ Custom Formats · Indexers · Download Clients · Import Lists
│                ├ Media Servers · Notifications · Requests · Users · Tags · Metadata
│                └ Security
└ System ────────┬ Status · Health · Tasks · Backup · Updates · Events · Logs
```

### Screen Catalog

| # | Screen | Route | Audience | Purpose & key components |
|---|---|---|---|---|
| 1 | **Setup Wizard** | `/setup` | First run | Create admin, connect media server (optional), add root folders, add download client, add indexers, pick default profiles. Every step can be skipped. |
| 2 | **Login** | `/login` | All | Local, Jellyfin, Emby, Plex and OIDC buttons, depending on what's enabled |
| 3 | **Dashboard** | `/` (admin) | Admin | Health alerts, active downloads, pending requests, upcoming (next 7 days), recently added, disk usage, indexer status, a "Getting started" checklist the admin can close |
| 4 | **Discover** | `/discover` | All | Hero carousel, horizontal rows (Trending, Popular Movies, Popular Series, Upcoming, Genres, Networks, Studios), availability badges on posters |
| 5 | **Media Detail (request view)** | `/media/movie/{tmdbId}`, `/media/tv/{tmdbId}` | All | Backdrop, metadata, cast, trailer, ratings, availability, **Request** button (season picker for TV), recommendations/similar, report issue |
| 6 | **Search Results** | `/search?q=` | All | Combined TMDB results with availability state |
| 7 | **Requests** | `/requests` | All (own) / Admin (all) | Filter by status/type/user; approve/decline inline; bulk actions; request detail drawer |
| 8 | **Issues** | `/issues`, `/issues/{id}` | All / Admin | Issue list, threaded comments, resolve/reopen |
| 9 | **Movie Library** | `/movies` | Admin | Poster / overview / table views; filters (monitored, missing, quality, tags, genre, year); sort; bulk editor (profile, root folder, monitored, tags, delete) |
| 10 | **Movie Detail (manage)** | `/movies/{id}` | Admin | Header with status/quality/size, file info and media info, actions (Search, Interactive Search, Refresh, Rename, Manual Import, Edit, Delete), history tab, cast/crew, collection link |
| 11 | **Add Movie / Series** | `/movies/add`, `/series/add` | Admin | Search box, results, add modal (root folder, profile, monitor option, min availability / series type, tags, "start search") |
| 12 | **Series Library** | `/series` | Admin | Same views as movies; progress bar (episodes have/total) |
| 13 | **Series Detail (manage)** | `/series/{id}` | Admin | Seasons accordion; episode table (monitor toggle, number, title, air date, status, quality, actions); per-season search; season pass; history |
| 14 | **Interactive Search** | modal on detail pages | Admin | Release table: protocol, age, title, indexer, size, peers, language, quality, CF score, **rejection reasons tooltip**, grab button, "override and grab" |
| 15 | **Collections** | `/movies/collections` | Admin | Franchise cards with owned/missing count, monitor collection |
| 16 | **Import Library** | `/movies/import`, `/series/import` | Admin | Scan a root folder, match folders to metadata, fix mismatches, bulk add |
| 17 | **Manual Import** | modal | Admin | Pick folder, file table with detected media/episode/quality/language, editable matches, rejection reasons, import mode (move/copy/hardlink) |
| 18 | **Calendar** | `/calendar` | Admin, or users with VIEW_CALENDAR | Month / week / agenda / day views; color by status; filters; iCal link. Calendar-only users see entries without links into the library. |
| 19 | **Queue** | `/activity/queue` | Admin, or users with VIEW_QUEUE (read-only) | Live progress, ETA, status messages, client, actions (remove, blocklist, manual import, change category). Read-only viewers see no actions or pending releases. |
| 20 | **History** | `/activity/history` | Admin | Event table with filters; expand for details (release data, paths) |
| 21 | **Blocklist** | `/activity/blocklist` | Admin | Entries with reasons; remove, clear |
| 22 | **Wanted: Missing / Cutoff Unmet** | `/wanted/*` | Admin | Paged tables, select & search, search all |
| 23 | **Indexers** | `/indexers` | Admin | Indexer cards/table with status, priority, protocol, tags; **Add Indexer** catalog modal (search 500+ definitions, filter by protocol/privacy/language); test |
| 24 | **Indexer Search** | `/indexers/search` | Admin | Free-text multi-indexer search with categories; grab to client directly |
| 25 | **Indexer Stats** | `/indexers/stats` | Admin | Charts: queries/grabs/failures over time, response time, per-indexer breakdown |
| 26 | **Settings → Profiles** | `/settings/profiles` | Admin | Quality profile editor (drag-order qualities, groups, cutoff, CF scores), delay profiles, release profiles |
| 27 | **Settings → Custom Formats** | `/settings/customformats` | Admin | CF list, condition builder, import/export JSON (TRaSH compatible), **test a release name** box |
| 28 | **Settings → Media Management / Naming** | `/settings/mediamanagement`, `/settings/naming` | Admin | Root folders, import mode, recycle bin, permissions (chmod/chown), naming templates with live preview |
| 29 | **Settings → Download Clients / Notifications / Media Servers / Import Lists** | `/settings/*` | Admin | Generic provider list + schema-driven form modal + Test |
| 30 | **Settings → Requests** | `/settings/requests` | Admin | Default profiles/roots per type (and 4K), auto-approve rules, default quotas, partial requests on/off, discovery region/language |
| 31 | **Settings → Users** | `/settings/users` | Admin | User table, import from Jellyfin/Plex, edit permissions/quotas, reset password, per-user notification settings |
| 32 | **Settings → General / Security** | `/settings/general` | Admin | Bind address, port, URL base, API key (regenerate), auth methods, OIDC config, proxy, logging level, analytics off |
| 33 | **User Profile** | `/profile` | All | Display name, avatar, password, linked accounts, notification prefs, quota usage, own API key |
| 34 | **System → Status / Health / Tasks / Backup / Updates / Logs / Events** | `/system/*` | Admin | Version info, health checks with links to fixes, scheduled tasks table + run now, backup list + restore, log viewer with level filter and live tail |
| 35 | **Books** *(M8)* | `/books/*` | Admin | Author library, author detail (books by series, ebook/audiobook status per book), add, import library |
| 36 | **Music** *(M9)* | `/music/*` | Admin | Artist library, artist detail (albums by type), album detail (tracks, chosen release), add, import library |

### Wireframes

**Dashboard (admin)**
```
┌──────────────────────────────────────────────────────────────────────────┐
│ ☰ Reelix      [ / Search movies, series, people…            ]   🔔 3  👤 │
├──────────┬───────────────────────────────────────────────────────────────┤
│ Discover │  ⚠ 1 indexer failing (NZBGeek) · Disk /media 92% full         │
│ Requests │ ┌───────────────┐ ┌───────────────┐ ┌───────────────┐         │
│ Issues   │ │ Downloading 4 │ │ Pending Req 7 │ │ Missing  132  │         │
│──────────│ └───────────────┘ └───────────────┘ └───────────────┘         │
│ Movies   │  Active Downloads                                             │
│ Series   │  Dune Part Two  2160p WEB-DL  ███████░░░ 71%  12m  qBit       │
│ Calendar │  Severance S02E07  1080p     ███░░░░░░░ 34%  25m  SAB         │
│ Activity │  Upcoming (7 days)          │  Recently Added                 │
│ Wanted   │  Mon  Show A S03E02         │  [poster][poster][poster]       │
│ Indexers │  Wed  Movie B (digital)     │                                 │
│ Settings │  Fri  Show C S01E09         │  Pending Requests  [Approve all]│
│ System   │                             │  alice → Movie X    ✓ ✕         │
└──────────┴───────────────────────────────────────────────────────────────┘
```

**Discover (requester)**
```
┌──────────────────────────────────────────────────────────────────────────┐
│ Reelix   Discover  Requests  Issues     [ Search…              ]   👤    │
├──────────────────────────────────────────────────────────────────────────┤
│ ███████████████  HERO BACKDROP — "Trending: Movie Title"  [Request]  ███ │
│                                                                          │
│ Trending                                                       See all → │
│ [▣ ✓Avail][▣ ⏳Req'd][▣      ][▣ ◐Partial][▣      ][▣      ]              │
│ Popular Series                                                 See all → │
│ [▣      ][▣ ✓Avail][▣      ][▣      ][▣ ⏳Req'd][▣      ]                │
│ Genres:  (Action) (Comedy) (Drama) (Sci-Fi) (Animation) (Horror) …       │
│ Networks: [HBO] [Netflix] [Apple TV+] [BBC] …                            │
└──────────────────────────────────────────────────────────────────────────┘
```

**Series Detail (manage)**
```
┌──────────────────────────────────────────────────────────────────────────┐
│ [poster]  Show Name (2022)   Continuing · HBO · 60m · TV-MA              │
│           Profile: HD-1080p · /tv/Show Name (2022) · 48.2 GB · tags: 4k? │
│           [🔍 Search Monitored] [Interactive] [⟳ Refresh] [✎ Rename] [⋯] │
├──────────────────────────────────────────────────────────────────────────┤
│ ▼ Season 2   ● monitored   8/10 episodes   [🔍] [👤]                     │
│   ● 10  Episode Title      2025-03-20  Unaired                 [🔍][👤]  │
│   ● 09  Episode Title      2025-03-13  Missing                 [🔍][👤]  │
│   ● 08  Episode Title      2025-03-06  WEBDL-1080p  2.1 GB  ✓  [🔍][👤]  │
│ ▶ Season 1   ● monitored   10/10 episodes                                │
│ ▶ Specials   ○ unmonitored 0/3                                           │
└──────────────────────────────────────────────────────────────────────────┘
```

**Interactive Search**
```
┌ Interactive Search — Movie Title (2024) ─────────────────────────────────┐
│ Filter: [All qualities ▾] [All indexers ▾] [Torrent+Usenet ▾]   38 found │
├────┬─────┬──────────────────────────────────┬────────┬──────┬────┬───────┤
│Prot│ Age │ Title                            │Indexer │ Size │Peer│CF│ ⬇  │
├────┼─────┼──────────────────────────────────┼────────┼──────┼────┼──┼────┤
│ 🧲 │ 2d  │ Movie.2024.2160p.UHD.BluRay.REM… │ IdxA   │ 58GB │ 41 │+150│⬇  │
│ 📰 │ 5d  │ Movie.2024.1080p.WEB-DL.DDP5.1…  │ IdxB   │ 6GB  │ —  │+25 │⬇  │
│ 🧲 │ 1d  │ Movie.2024.1080p.CAM.x264-…   ⚠  │ IdxC   │ 2GB  │ 300│-1000│⬇ │
│    │     │  └ Rejected: Quality CAM not wanted in profile; CF score < min   │
└────┴─────┴──────────────────────────────────┴────────┴──────┴────┴──┴────┘
```

### UI Component Library (Leptos)

These shared components live in `reelix-web/src/components`: `PosterCard`, `PosterGrid` (virtualized), `DataTable` (sortable, selectable, virtualized, column chooser), `ProviderForm` (renders from schema), `Modal`, `Drawer`, `Toast`, `ProgressBar`, `StatusBadge`, `QualityBadge`, `TagInput`, `SeasonPicker`, `CalendarView`, `Chart` (wrapping a lightweight chart lib), `LogViewer`, `ConfirmDialog`, `EmptyState`, `Skeleton`.

Internationalization uses `leptos_i18n` with Fluent-style message files, English first.

---

## Users, Roles & Permissions

Permissions are **bitflags**. Roles are presets that you can customize per user.

| Permission | Admin | Manager | Power User | User |
|---|:-:|:-:|:-:|:-:|
| ADMIN (everything, settings, system) | ✓ | | | |
| MANAGE_LIBRARY (add/edit/delete media, search, grab) | ✓ | ✓ | | |
| MANAGE_REQUESTS (approve/decline/edit) | ✓ | ✓ | | |
| MANAGE_USERS | ✓ | ✓ | | |
| MANAGE_ISSUES | ✓ | ✓ | | |
| REQUEST_MOVIE / REQUEST_TV | ✓ | ✓ | ✓ | ✓ |
| REQUEST_4K | ✓ | ✓ | ✓ | |
| REQUEST_ADVANCED (choose profile/root) | ✓ | ✓ | ✓ | |
| AUTO_APPROVE (movie / tv / 4k) | ✓ | ✓ | ✓ | |
| VIEW_REQUESTS (all users) | ✓ | ✓ | ✓ | |
| CREATE_ISSUES | ✓ | ✓ | ✓ | ✓ |
| VIEW_CALENDAR / VIEW_QUEUE | ✓ | ✓ | opt | opt |
| REQUEST_BOOK / REQUEST_MUSIC | ✓ | ✓ | ✓ | opt |
| REQUEST_RADIO / AUTO_APPROVE_RADIO | ✓ | ✓ | ✓ | opt |

"opt" means off in the preset, but it can be ticked per user. VIEW_CALENDAR opens the calendar, and VIEW_QUEUE opens a read-only download queue. Managing queue items still needs MANAGE_LIBRARY.

Permissions are enforced in the **service layer**, not just in handlers or the UI.

---

## Configuration

Precedence: **CLI flags > environment variables > `config.toml` > defaults.** Most settings live in the database and are edited in the UI. The config file covers bootstrap options only.

```toml
# /config/config.toml
[server]
bind = "0.0.0.0"
port = 7700
url_base = ""            # e.g. "/reelix" behind a reverse proxy

[database]
url = "sqlite:///config/reelix.db"   # or postgres://user:pass@host/reelix

[paths]
data_dir = "/config"
backup_dir = "/config/backups"
log_dir = "/config/logs"

[security]
# Used for encrypting provider secrets at rest; auto-generated on first run if absent
secret_key_file = "/config/secret.key"
trusted_proxies = ["172.16.0.0/12"]

[logging]
level = "info"           # trace | debug | info | warn | error
format = "pretty"        # pretty | json
```

| Env var | Purpose |
|---|---|
| `REELIX_PORT`, `REELIX_BIND`, `REELIX_URL_BASE` | Server |
| `REELIX_DATABASE_URL` | DB connection |
| `REELIX_DATA_DIR` | Config/data root |
| `REELIX_SECRET_KEY` | Overrides key file |
| `REELIX_LOG_LEVEL` | Logging |
| `REELIX_TMDB_API_KEY` | Optional own TMDB key (a default app key ships) |
| `PUID`, `PGID`, `UMASK`, `TZ` | Docker conventions |

---

## Deployment

### Docker (primary)

```yaml
# docker-compose.yml
services:
  reelix:
    image: ghcr.io/demonzoned/reelix:latest
    container_name: reelix
    environment:
      - PUID=1000
      - PGID=1000
      - TZ=America/New_York
    volumes:
      - ./config:/config
      - /data:/data           # single mount containing torrents/, usenet/, media/ → enables hardlinks
    ports:
      - "7700:7700"
    restart: unless-stopped
```

**Recommended folder structure** (so hardlinks and atomic moves work):

```
/data
├── torrents/{movies,tv,books,music}
├── usenet/{movies,tv,books,music}
└── media/{movies,tv,books,audiobooks,music}
```

### Native binaries

Native binaries are built for `x86_64` and `aarch64` on Linux (musl, static), Windows and macOS. They ship with a systemd unit, a Windows service wrapper and a launchd plist. `ffprobe` must be on `PATH` (the Docker image bundles it).

### Reverse proxy

Reelix supports `url_base`, `X-Forwarded-*` headers from trusted proxies and WebSocket upgrades. The docs include example configs for Nginx, Caddy, Traefik and SWAG.

### Image

The image is a multi-stage build (`cargo chef` for layer caching) on a distroless or Alpine base with ffprobe. The target size is under 80 MB.

---

## Migrating from the *arr Stack

`reelix-migrate` (UI: **Settings → Import from…**, CLI: `reelix migrate --from radarr --url … --api-key …`):

| Source | Imported |
|---|---|
| Radarr | Movies (with file records and paths, no file moves), quality profiles, custom formats, naming, root folders, tags, download clients, notifications, history (optional), blocklist |
| Sonarr | Series, seasons and episode monitoring, episode files, profiles, CFs, naming, tags, clients, notifications, history (optional) |
| Prowlarr | Indexers (definitions and settings), proxies, tags, app sync is discarded |
| Jellyseerr / Overseerr | Users (with media-server links), requests (with status), issues, request settings, quotas |
| Readarr | Authors, books and monitoring, book files, quality/metadata profiles, naming, root folders, tags |
| Lidarr | Artists, albums (with chosen releases) and monitoring, track files, quality/metadata profiles, naming, root folders, tags |

The import runs through the **API** of the running source app, with a fallback that reads the SQLite DB directly. It includes a dry-run report with conflicts and is idempotent, so it's safe to re-run.

---

## Current Progress

*Last updated 2026-10-31.*

**Working (movies and TV, M0–M7):** setup wizard, login and users; quality profiles, custom formats and the decision engine; movie and series libraries with scan, import, rename and metadata refresh; Torznab/Newznab indexers with health, stats and manual search; SABnzbd, NZBGet, qBittorrent, Transmission, Deluge and blackhole clients; the search → grab → track → import pipeline with manual import, history and blocklist; calendar and iCal feed; discovery, requests, approvals, quotas and issues; Jellyfin/Emby/Plex login and refresh; eleven notifiers; import lists and watchlist sync.

**Working (books and music, M8–M9):** authors, books, editions and series with separate ebook/audiobook monitoring, profiles and root folders; artists, albums, release choice and tracks with metadata profiles; Open Library and MusicBrainz/Cover Art Archive metadata (rate-limited and cached); book and music release parsers (175 and 281 fixtures, 100% pass); ebook, audiobook and music quality families with default profiles and size limits; book/music indexer categories and search modes, and book/music download-client categories; automatic, RSS and interactive search, grab, tracking, atomic import with EPUB/M4B/audio tag and cover writing (copying instead of hardlinking when tags are written), history and blocklist; library scan, import and rename; book and album requests with formats, quotas and auto-approve; Audiobookshelf, Kavita, Calibre-Web and Navidrome refresh; Last.fm, ListenBrainz and Spotify import lists.

**Recently fixed:**
- The VIEW_CALENDAR and VIEW_QUEUE permissions now work for non-manager users (power users and users). Before, the calendar and queue were only reachable with MANAGE_LIBRARY.
- The dashboard's "Getting started" card can be closed. The choice is remembered per browser.

**Known issues:**
- An approved movie request was not searched for automatically, and its request did not move from *Requested* to *Available* after the download finished. The approval code does queue a search and the status sync runs every 15 minutes, so the cause is not yet known. Investigating needs the server log or database from an affected run.

**Open gaps in finished milestones:**
- Parser: 624 fixtures (100% pass) against a 2,000 target, and no `cargo fuzz` target yet.
- Indexers: the YAML definition engine works, but no definitions ship yet.
- TV: the XEM/scene-mapping sync job isn't written.
- Notifications: per-user preferences are stored, but request notifications still go to every notifier.
- Requests: 4K availability isn't tracked separately.
- CI and the Docker image are written but haven't run in CI yet.
- Nothing has been tested against real indexers, download clients or media servers.

**Gaps in books and music:**
- No manual import for book or album downloads; failed imports can be retried from the queue.
- Calibre-Web can only be reconnected (best effort); Audiobookshelf and Kavita rescan whole libraries.
- Spotify list items match by name only; there are no book import lists.
- Discover shows artists as chips without an album browser; there's no bulk approve for book/album requests.
- Metadata is cached in memory only, so a restart refetches from MusicBrainz at 1 request/second.
- YAML-definition indexers always use the default book/music categories.
- Scene music names with hyphenated artists (for example "Alt-J") split wrongly.
- None of it has been tested against real indexers, download clients, media servers or a large real library, so the M8/M9 exit criteria are unverified.

**System screens (done ahead of M10):** scheduled and manual backups (`VACUUM INTO` snapshot + `config.toml` + secret key in a zip, retention for scheduled ones), download, delete and restore (stored or uploaded; staged and applied on the next start before the database opens, rolled back if the restored database won't open); a log viewer over the rolling log files (pretty and JSON) with level/text filters, a live tail and downloads; update checks against the `demonzoned/Reelix` GitHub releases (twice a day when enabled, with a health notice; no self-update); editing bind address, port, URL base and logging in `config.toml` from Settings → General; and per-user UI settings (date/time format, relative dates, first day of week, default calendar and library views).

**Migration, compatibility and hardening (M10):** the `reelix-migrate` tool (System → Migrate) for Radarr, Sonarr, Prowlarr, Readarr, Lidarr and Jellyseerr; a Torznab/Newznab aggregator; Radarr/Sonarr/Readarr/Lidarr-compatible APIs; a performance pass (lists filter and sort on light rows and load only the page; all library endpoints under 150 ms p95 with 10k movies, 100k episodes and 100k tracks); an accessibility pass (contrast, focus handling in dialogs, keyboard access, landmarks, page titles, status text); and a security review with fixes, a threat model ([security.md](security.md)), property tests and libFuzzer targets for every parser.

**Not started:** PostgreSQL support (all ~470 queries are written for SQLite).

---

## Milestones & Roadmap

Each milestone ends with a tagged pre-release and a working Docker image. Books and music come next, so migration, compatibility and hardening (M10) cover all six tools in one pass.

### M0 — Foundation *(weeks 1–3)*
- [x] Cargo workspace, CI (fmt, clippy -D warnings, test, cargo-deny, cargo-audit), Docker build
- [x] `reelix-core` entities and events; `reelix-db` with SQLite migrations and repository pattern
- [x] axum server, config loading (figment), tracing, graceful shutdown
- [x] Auth: local users, argon2, sessions, API keys; setup-wizard API
- [x] Persistent job queue and scheduler skeleton
- [x] Event bus and WebSocket hub
- [x] Leptos app shell: layout, nav, login, setup wizard, theme, toasts
- [x] OpenAPI generation and `/api/docs`

**Status: ✅ complete.** Docker image and CI workflow are written but not yet run in CI.

**Exit criteria:** A user can install via Docker, complete the setup wizard, log in and see an empty dashboard. CI is green.

### M1 — Release Parser & Decision Engine *(weeks 3–6)*
- [ ] `reelix-parser` with 2,000+ fixture titles (movies + TV + anime), fuzzing — *624 fixtures (100% pass) + a never-panic test; still short of 2,000, and no `cargo fuzz` target yet*
- [x] Quality definitions and quality profiles (CRUD + UI editor)
- [x] Custom formats with all specification types, a "test release name" tool, TRaSH JSON import
- [x] Delay profiles, release profiles, blocklist model
- [x] Specification framework that returns structured rejection reasons

**Status: ✅ done** except the larger fixture corpus and fuzz target.

**Exit criteria:** Pasting any release name in the UI shows the parsed fields, matched CFs, score and accept/reject result against a profile. Fixture pass rate is at least 99%.

### M2 — Movies Core (Radarr parity, part 1) *(weeks 6–10)*
- [x] TMDB metadata provider with caching and rate limiting
- [x] Root folders, add movie, movie library (grid/table, filters, bulk edit), movie detail
- [x] Library scan and import of existing folders; ffprobe media info
- [x] Naming engine with movie tokens and preview; rename
- [x] Refresh-metadata job; collections

**Status: ✅ done.** Not yet measured against a real 1,000-movie library.

**Exit criteria:** A user can import an existing 1,000-movie library, see correct metadata and file info, and rename files safely, with the preview matching the result.

### M3 — Indexers (Prowlarr parity) *(weeks 9–13)*
- [x] Torznab and Newznab adapters (caps, search modes, categories)
- [x] YAML definition engine (Cardigann-compatible subset: login, search paths, selectors, filters) — *no definitions shipped yet (drop files into `{data_dir}/definitions`); pagination, `rows.after`/`dateheaders` not supported*
- [x] Rate limiting, back-off, health and stats
- [x] Proxy and FlareSolverr support
- [x] Indexer UI: catalog, add/test, stats charts, manual multi-indexer search
- [x] RSS sync job

**Status: ✅ engine and UI done.** The "50 definitions" exit criterion is not met: no definitions ship yet.

**Exit criteria:** At least 50 popular public and private definitions pass the test suite with recorded fixtures, and manual search works across five or more indexers concurrently.

### M4 — Download Clients & Import Pipeline *(weeks 12–16)*
- [x] **SABnzbd adapter** (full spec in [Download Client Integrations](#sabnzbd-usenet)): add via upload/URL, queue + history tracking, failure classification, health checks
- [x] qBittorrent, Transmission, Deluge, NZBGet, blackhole adapters
- [x] Search → decision → grab flow; interactive search UI with rejection reasons
- [x] Queue tracking, completed and failed download handling, remote path mappings
- [x] Import: hardlink/move/copy, upgrades, recycle bin, permissions
- [x] Manual import UI; history and blocklist UI
- [x] Missing and cutoff-unmet search

**Status: ✅ done** and covered by integration tests against mocked indexers and clients. Not yet run against real qBittorrent/SABnzbd instances.

**Exit criteria:** End-to-end, adding a movie leads to an automatic search, grab, download, import, rename and history entry with no manual steps, over both qBittorrent (torrent) and SABnzbd (Usenet). A failed download leads to a blocklist entry and a new search.

### M5 — TV Series (Sonarr parity) *(weeks 16–22)*
- [x] Series/season/episode model; TMDB TV (+ optional TVDB) provider
- [x] Monitoring options, season pass, series types (standard/daily/anime)
- [x] Episode parsing and matching: multi-episode, season packs, daily, absolute numbering
- [ ] XEM/scene mapping sync — *XEM client and scene-numbering columns exist; the sync job that fills them is not written yet*
- [x] Series library, series detail, episode actions, TV naming tokens
- [x] Calendar (movies + TV) and iCal feed

**Status: ✅ done** except XEM sync. Not yet benchmarked on a 300-series library.

**Exit criteria:** Season packs, multi-episode files and anime absolute releases import correctly against the fixture suite, and a 300-series library refreshes in under 5 minutes.

### M6 — Requests & Discovery (Jellyseerr parity) *(weeks 20–26)*
- [x] Discover feeds, search with availability, media detail (request view)
- [x] Request flow, approvals, decline reasons, quotas, 4K requests (second profile/root set) — *4K availability is not tracked separately yet*
- [x] Jellyfin, Emby and Plex login and user import; OIDC
- [x] Roles and permissions UI; user profile page
- [x] Issues with comments
- [x] Requester-only UI mode

**Status: ✅ done.**

**Exit criteria:** A Jellyfin user logs in, requests a series' season 2, an admin approves, and the user is notified when it becomes available. Quotas are enforced.

### M7 — Integrations & Notifications *(weeks 24–28)*
- [x] Media server library refresh and availability sync
- [x] Notifiers: Discord, Telegram, Email, Pushover, Gotify, ntfy, Apprise, Webhook, Slack, Matrix, custom script
- [ ] Per-user notification preferences — *preferences are stored and editable, but request notifications still go to every notifier*
- [x] Import lists (TMDB, Trakt, IMDb, MDBList, Plex/Jellyfin watchlists)

**Status: ✅ done** except per-user notification delivery.

**Exit criteria:** Every event type can be delivered to every notifier, and watchlist items automatically become requests.

### M8 — Books (Readarr parity)
- [x] `MediaKind::Book` across library, requests, queue, history and root folders
- [x] Open Library metadata provider (+ optional Google Books / Hardcover), with caching
- [x] Authors, books, editions and series; monitoring options; ebook and audiobook formats per book
- [x] Book/audiobook release parser with its own fixture suite
- [x] Book quality profiles, search → grab, import with EPUB/M4B metadata and cover writing, naming tokens
- [x] Author library, author/book detail, add and import-library screens
- [x] Book requests in Discover (REQUEST_BOOK, quotas, auto-approve)
- [x] Audiobookshelf and Calibre-Web/Kavita library refresh

**Status: ✅ implemented**, with the gaps listed under [Current Progress](#current-progress). Not yet run against real services.

**Exit criteria:** An existing ebook and audiobook library imports with correct metadata, and requesting a book leads to an automatic search, grab, import and rename.

### M9 — Music (Lidarr parity)
- [x] `MediaKind::Music` across library, requests, queue, history and root folders
- [x] MusicBrainz + Cover Art Archive provider, rate-limited and cached
- [x] Artists, release groups, releases and tracks; metadata profiles (album types); monitoring options
- [x] Music release parser with its own fixture suite (including discography packs)
- [x] Lossy/lossless quality profiles, search → grab, track matching, tag writing, naming tokens
- [x] Artist library, artist/album detail, release chooser, add and import-library screens
- [x] Album requests in Discover (REQUEST_MUSIC, quotas, auto-approve)
- [x] Navidrome library refresh; list imports (Last.fm, ListenBrainz, Spotify)

**Status: ✅ implemented**, with the gaps listed under [Current Progress](#current-progress). The 95% match rate hasn't been measured on a real library.

**Exit criteria:** A 20k-track library imports and matches at least 95% of albums automatically, and a requested album is searched, grabbed, tagged and imported with no manual steps.

### M10 — Migration, Compatibility & Hardening
- [x] `reelix-migrate` for Radarr, Sonarr, Prowlarr, Jellyseerr, Readarr and Lidarr — *over each app's API, with preview, path rewrites and hidden-secret entry; Readarr authors are matched on Open Library by name. Tested against mocked APIs only*
- [x] Radarr/Sonarr v3 and Readarr/Lidarr v1 API shims (subset needed by Bazarr, Recyclarr, Homepage, LunaSea) — *at `/radarr`, `/sonarr`, `/readarr`, `/lidarr`; Readarr/Lidarr are mostly read-only*
- [x] Torznab aggregator endpoint — *`/api/torznab/{all|id}` and `/api/newznab/{all|id}`, with proxied, sealed download links*
- [ ] PostgreSQL support and CI matrix
- [x] Backups/restore, update checks, log viewer, server settings — *restore needs a restart; updates are checked, not installed*
- [x] Performance pass (10k movies / 2k series / 100k tracks fixtures), accessibility pass (WCAG AA) — *`cargo test --release -p reelix-api --test perf -- --ignored`: every library endpoint < 150 ms p95; accessibility fixes from a code audit, not yet checked with a screen reader*
- [x] Security review, threat model, fuzzing on all parsers (XML, YAML, release names) — *see [security.md](security.md); four low findings open*

**Exit criteria:** A real user's full *arr stack migrates with no data loss, the performance targets are met, and there are no known critical bugs.

### M11 — 1.0 Release
- [ ] Documentation site (mdBook): install, migration, every setting, API, provider authoring guide
- [ ] Stable API v1 freeze and semver policy
- [x] Release automation: changelog, multi-arch images, binaries, checksums, SBOM — *`.github/workflows/release.yml` on `v*` tags: Windows x86_64 and Linux x86_64/aarch64 (glibc 2.17) archives, a `linux/amd64` + `linux/arm64` image on GHCR, `SHA256SUMS`, an SPDX SBOM, notes from CHANGELOG.md; not yet run on GitHub*

### Post-1.0 Ideas
- Subtitles (Bazarr-like), using OpenSubtitles and others
- Smart cleanup rules (watched + age + rating → delete, with dry-run)
- Multiple instances (e.g. 4K) through profiles instead of separate installs (partly covered by 4K profiles in M6)
- Comics and manga
- Mobile PWA with push notifications
- Plugin system (WASM components) for third-party providers

---

## Testing Strategy

| Level | Tooling | Scope |
|---|---|---|
| Unit | `cargo test`, `rstest`, `proptest` | Parser, decision specs, naming engine, quota math |
| Fixture/golden | `insta` snapshots | Release names → parsed output; naming previews; indexer XML → ReleaseInfo |
| Fuzz | `cargo-fuzz` | Release parser, Torznab XML, YAML definitions, filename matcher |
| Integration | `sqlx::test`, `wiremock` | Repos on SQLite and Postgres; indexer/client/metadata adapters against mocked HTTP |
| Contract | Recorded real responses | qBittorrent/SAB/Transmission API versions; TMDB payloads |
| Filesystem | temp dirs (`tempfile`), loopback FS for hardlink tests | Import modes, permissions, recycle bin, cross-device fallback |
| End-to-end | Playwright + docker-compose (Reelix + qBittorrent + SABnzbd + mock Torznab/Newznab indexers + Jellyfin) | Critical user journeys per milestone exit criteria |
| Performance | `criterion`, k6 for API | Parser throughput, library list endpoints, WS fan-out |

CI gates: fmt, clippy (pedantic where sensible), tests on Linux/macOS/Windows, `cargo deny` (licenses and advisories), coverage report (target ≥ 80% on core crates).

---

## Security

- Auth is required by default. A "disable auth for local addresses" option exists but is **off** by default.
- Passwords are hashed with argon2id. API keys are stored hashed. Sessions use HttpOnly, Secure and SameSite=Lax cookies.
- CSRF protection on cookie-authenticated state-changing requests.
- Provider secrets are encrypted at rest (AES-256-GCM).
- Logins are rate-limited, with lockout back-off.
- Strict CSP headers. The UI loads no third-party scripts, and TMDB images go through an optional image proxy/cache.
- **Path safety:** every filesystem operation is restricted to configured root folders and download paths. Paths are canonicalized and symlink escapes rejected. Reelix never deletes outside its roots.
- Release-group, title and other external strings are always escaped in the UI. Custom-script notifications receive data through environment variables, never a shell-interpolated string.
- SSRF guard on user-supplied URLs (indexers, webhooks), with an opt-in to allow private IPs since most homelab targets are LAN.
- Dependency auditing in CI, plus signed releases and an SBOM.

---

## Performance Targets

| Metric | Target |
|---|---|
| Idle memory | < 100 MB (10k movies, 2k series) |
| Cold start | < 1 s (excluding migrations) |
| Movie library page (10k items) API | < 150 ms p95 |
| UI first contentful paint | < 1.5 s on LAN |
| Release parse throughput | > 50k titles/s single core |
| Concurrent indexer search | 20 indexers, results streamed as they arrive, total bounded by slowest indexer timeout (default 30 s) |
| Queue poll overhead | < 1% CPU with 50 active downloads |
| Music library *(M9)* | 2k artists / 100k tracks without slowing the artist list (< 150 ms p95) |

---

## Observability

- Structured logs (`tracing`) to the console and rolling files, plus a UI log viewer with live tail.
- Optional `/metrics` Prometheus endpoint: job durations, queue depth, indexer latency/failures, HTTP latency and grabs/imports counters.
- Optional OpenTelemetry trace export.
- `/api/v1/system/health/live` for container health checks (`reelix healthcheck`). It checks only the database and the data directory, and returns 200, or 503 when either fails. `/api/v1/system/health` is the full report, download clients and indexers included; it returns 503 when any check is an error, so the container does not use it.

---

## Development Setup

```bash
# Prerequisites
rustup toolchain install stable
rustup target add wasm32-unknown-unknown
cargo install --locked trunk

# Backend (API on :7700; migrations run automatically on startup, data in ./data)
cargo run -p reelix-server

# Frontend with hot reload (second terminal; :8080 proxies /api to :7700)
cd crates/reelix-web && trunk serve

# Or build the UI once and let the server host it at http://localhost:7700
(cd crates/reelix-web && trunk build --release) && cargo run -p reelix-server

# Tests
cargo test --workspace
cargo clippy -p reelix-web --target wasm32-unknown-unknown -- -D warnings
# Later milestones: cargo xtask fixtures check (parser golden tests), npx playwright test (e2e)

# Lint
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
```

A dev compose stack in `tests/e2e/compose.yml` brings up qBittorrent, SABnzbd, mock Torznab and Newznab indexers, and Jellyfin with sample media for local end-to-end work.

---

## Contributing

- Use Conventional Commits (`feat(indexers): add Deluge v2 support`).
- Open an issue or discussion before large features. Architecture changes need a short ADR in `docs/adr/`.
- New providers need an adapter, a settings schema, recorded fixtures, tests and docs.
- New parser cases go in as fixtures **before** the fix.

---

## Open Questions

| # | Question | Leaning |
|---|---|---|
| 1 | Leptos vs. TypeScript SPA for the frontend | Leptos; re-evaluate at end of M1 |
| 2 | TMDB-only for TV, or require TVDB? | TMDB primary (free), TVDB optional for better anime/scene matching |
| 3 | Ship our own TMDB API key or require the user's? | Ship a default key, allow override |
| 4 | Indexer definitions: fork the Prowlarr/Jackett YAML catalog or maintain our own? | Consume the upstream catalog where licensing allows, with a compatibility layer |
| 5 | 4K handling: separate profiles/roots in one instance vs. linked instances | Single instance with a "4K" variant per movie/series |
| 6 | SQLite concurrency at scale (WAL mode + single writer task) vs. pushing Postgres | WAL + write-serialization; Postgres optional |
| 7 | Plugin system (WASM) timing | Post-1.0 |
| 8 | License: GPL-3.0 vs. AGPL-3.0 vs. MIT/Apache | See below |
| 9 | Book metadata source (Readarr's own source is no longer maintained) | Open Library by default; Google Books and Hardcover as optional providers |
| 10 | Ebooks and audiobooks: one book with two formats, or two separate libraries? | One book with per-format monitoring, profiles and root folders |
| 11 | MusicBrainz: call the public API directly, or run/offer a mirror? | Public API with aggressive caching; optional self-hosted mirror URL |
| 12 | Should audio tagging rewrite files that are still seeding? | No. Tag the imported copy only, and refuse to tag hardlinked files unless the user opts in |

---

## License & Legal

**License:** to be decided before the first public release. GPL-3.0 is the leading candidate, in line with the ecosystem and keeping the option to reuse GPL-licensed indexer definitions open.

Reelix is a media management and automation tool. It doesn't host, index or distribute content. Users are responsible for complying with the laws of their jurisdiction and the terms of any services they connect.

TMDB attribution: *This product uses the TMDB API but is not endorsed or certified by TMDB.*
