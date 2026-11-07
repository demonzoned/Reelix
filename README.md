# Reelix

**One self-hosted app for your whole media library: discover, request, search, download, import and organize movies, TV, books, audiobooks, music, and radio shows and podcasts.**

Reelix does the jobs of Radarr, Sonarr, Prowlarr, Jellyseerr, Readarr and Lidarr in a single Rust service with a single web UI. You get one binary, one database, one login and one API, with no API keys to wire between apps and no state to keep in sync.

| Area | What Reelix does |
|---|---|
| **Movies & TV** | Monitors movies and series, searches indexers, grabs releases, imports and renames files |
| **Books & audiobooks** | Monitors authors and books, with ebooks and audiobooks as separate formats of one book |
| **Music** | Monitors artists and albums, matches releases against MusicBrainz, tags and renames tracks |
| **Radio & podcasts** | Follows radio shows and podcasts, keeps their series and episodes, and files new episodes into an Audiobookshelf podcast library |
| **Indexers** | Torznab and Newznab indexers, Cardigann-style definitions and Soulseek (through slskd), with health checks, statistics and a manual multi-indexer search |
| **Requests** | A discovery and request portal with users, approvals, quotas and media-server login |

---

## Features

### Library
- Add movies, series, authors, books, artists and albums by search, by ID (TMDB, TVDB, IMDb, ISBN, MusicBrainz) or from import lists.
- Flexible monitoring: minimum availability for movies; all, future, missing, first or latest season, or per season/episode for series; all, future or selected books and albums.
- Standard, daily and anime series, with absolute numbering (most accurate with a TheTVDB key).
- Root folders for every library on one page, with free-space and accessibility checks.
- Library scans that match existing folders and files, and an import page for unmapped folders: folders are listed right away, matched in the background with a progress counter, and imported in batches, so large libraries work behind a reverse proxy too.
- Library, activity and request pages load more as you scroll, and going back returns you to where you were.
- Tokenized naming templates for every media type, with a live preview for movies and TV.
- Hardlink, copy or move imports. Hardlinks are preferred so torrents keep seeding, and every import is written to a temporary file first and then renamed into place.
- Recycle bin instead of hard deletes.
- Calendar of upcoming releases, with an iCal feed.
- Wanted views for missing items and items below their quality cutoff.

### Quality and decisions
- Quality definitions with size limits, and quality profiles with ordered and grouped qualities, an upgrade cutoff and upgrade toggles.
- Ebook (EPUB, AZW3, PDF…), audiobook (M4B, MP3…) and lossy/lossless music quality families.
- Custom formats with conditions on title, release group, source, resolution, codec, HDR, audio, language, edition, indexer flags and size, each with a score. Import and export as JSON (TRaSH-guide compatible).
- Delay profiles to prefer Usenet or torrents, or to wait for a better release.
- Release restrictions (must / must not contain) and a blocklist.
- A release-name parser for movies, TV, books and music, with a page to test how a name is parsed.

### Indexers and download clients
- Torznab and Newznab indexers, plus YAML (Cardigann-style) indexer definitions.
- Rate limiting, back-off, and automatic disabling of failing indexers. Per-indexer categories, priorities, tags and seed goals.
- HTTP and SOCKS5 proxies, and FlareSolverr for Cloudflare-protected sites.
- **Torrent clients:** qBittorrent, Transmission, Deluge, rTorrent and a blackhole folder.
- **Usenet clients:** SABnzbd, NZBGet and a blackhole folder.
- **Soulseek** through [slskd](https://github.com/slskd/slskd), as both an indexer and a download client, for music, books, audiobooks and radio; see [Soulseek setup](docs/soulseek.md).
- Automatic import of finished downloads, failed-download handling with blocklisting and a new search, and remote path mappings for clients on another host.
- **Direct downloads:** native Rust Spotify tracks, albums and playlists, plus YouTube audio or video with format/resolution selection, playlist preview, a persistent queue, cancellation and retry. Set up tools and destinations on **Downloads**; see [direct download setup](docs/direct-downloads.md).

### Discover and requests
- Discover page with trending, popular and upcoming movies and TV.
- Books and music on Discover too: trending books and books by subject (fantasy, science fiction, mystery and more) from Open Library, and new releases and popular albums from ListenBrainz, plus search for both.
- Radio and podcasts on Discover: Apple Podcasts top charts and categories for your country, BBC Sounds popular programmes overall, per station and in 32 categories, and Podcast Index trending.
- Shows availability everywhere: available, partially available, requested, pending or processing.
- Request movies, whole series or single seasons, books (ebook, audiobook or both), albums and radio shows.
- Approval workflow with auto-approve per user or permission, per-user quotas, and decline reasons.
- Sign in with local accounts, Jellyfin, Emby or Plex accounts, or OIDC.
- Users can report issues (audio, video, subtitles, other) on available media.
- Plex watchlist sync (each user's watchlist becomes requests), and Plex and Jellyfin watchlists as import lists.

### Radio and podcasts
- Follow shows from podcast feeds, BBC programmes, Apple Podcasts, Podcast Index and the Internet Archive, by search or by pasting a link.
- Follow every episode, only new ones, or none. Reelix keeps each show's series and episodes, numbering episodes in broadcast order when the source doesn't.
- Matches episodes to the files in your Audiobookshelf podcast library (by source id, `S01E02` or title), with manual matching and a review page for anything that doesn't match.
- Searches your indexers (and Soulseek) for wanted episodes and files them into the podcast library with tags, without re-encoding, then asks Audiobookshelf to scan.
- Show artwork and station logos are downloaded and served by Reelix.

### Metadata
- Movies and TV from TMDB, optionally TheTVDB.
- Books from Open Library, with metadata and cover art written into imported EPUBs and audiobooks.
- Music from MusicBrainz and the Cover Art Archive, with tags and cover art written into imported tracks.
- Reelix only writes into files it owns: when metadata is written, imports copy instead of hardlinking, so files that are still seeding are never modified.
- Optionally adds imported ebooks to a Calibre library.

### Integrations
- **Media servers:** Jellyfin, Emby and Plex (library refresh and availability), plus Audiobookshelf (books and podcast libraries), Kavita, Calibre-Web and Navidrome.
- **Import lists:** TMDB, Trakt, IMDb, MDBList, Plex and Jellyfin watchlists, other Reelix instances, URL lists, and Last.fm, ListenBrainz and Spotify for music.
- **Notifications:** Discord, Slack, Telegram, email (SMTP), Pushover, Gotify, ntfy, Apprise, Matrix, webhooks and custom scripts.

### System
- Health checks, a task scheduler with manual triggers, an event log and activity history.
- Scheduled and manual backups of the database, `config.toml` and secret key, with download, retention and one-click restore (applied on the next restart, and rolled back if the restored database won't open).
- A log viewer with level and text filters, a live tail and log file downloads.
- Update checks against the [GitHub releases](https://github.com/demonzoned/Reelix/releases), with release notes and a health notice when a new version is out, and one-click (or automatic) install and restart for the Windows and Linux releases.
- Bind address, port, URL base and logging can be edited in Settings → General; they are saved to `config.toml` and apply after a restart.
- Per-user UI settings: date and time format, relative dates, first day of the week, and default calendar and library views.
- REST API with API keys, and interactive API documentation at `/api/docs`.

### Security
- A one-time setup token for first-run setup from another machine, login rate limits, and trusted-proxy support.
- Tracker cookies, passkeys and other credentials encrypted at rest; backups and keys readable only by Reelix.
- Server-side requests to user-supplied addresses can't reach private networks unless you allow it.
- Signed release builds, pinned build dependencies, and checksums verified by the installers and in-app updates.

### Migration and compatibility
- **Migrate from the \*arr stack** (System → Migrate): connect to Radarr, Sonarr, Prowlarr, Readarr, Lidarr or Jellyseerr with its URL and API key, preview what will be added, reused or skipped, then import tags, root folders, quality profiles, indexers, download clients, remote path mappings, movies, series, authors, artists, users and requests. Path rewrites cover different volume mounts, hidden passwords can be entered in the preview, and running it again doesn't duplicate anything.
- **Use Reelix as an indexer**: `/api/torznab/all` (torrents) and `/api/newznab/all` (Usenet) search every indexer configured in Reelix; add them to other apps as one Torznab/Newznab indexer. Download links go through Reelix, so passkeys never leave it.
- **\*arr-compatible APIs** for tools such as Homepage, Bazarr, LunaSea, nzb360 and Recyclarr: point a Radarr client at `http://reelix:7700/radarr`, a Sonarr client at `/sonarr`, Readarr at `/readarr` and Lidarr at `/lidarr`, with a Reelix API key.

---

## Installation

Release archives, installers and `SHA256SUMS` carry signed build provenance; check a download with `gh attestation verify <file> --repo demonzoned/Reelix`.

### Docker

```yaml
# docker-compose.yml
services:
  reelix:
    image: ghcr.io/mvanhare/reelix:latest
    container_name: reelix
    environment:
      - PUID=1000
      - PGID=1000
      - UMASK=022
      - TZ=America/New_York
    volumes:
      - ./config:/config
      - /data:/data   # one mount holding downloads and media, so hardlinks work
    ports:
      - "7700:7700"
    restart: unless-stopped
```

```bash
docker compose up -d
```

To build the image yourself, run `docker build -f docker/Dockerfile -t reelix .` from the repository root. The image includes the web UI and runs as `PUID:PGID`. Add `--build-arg REELIX_WEB_FEATURES=classic-setup` to build it with the classic onboarding instead of the set-up wizard.

On a server with no desktop, follow [Set up Reelix from a terminal only](docs/terminal-setup.md).

### Windows

**Installer (recommended):** download `reelix-<version>-windows-x86_64-setup.exe` and run it. Setup asks how to install:

- **Install for me only** (no admin rights): installs to `%LOCALAPPDATA%\Programs\Reelix` and keeps data in `%LOCALAPPDATA%\Reelix`. Start Reelix from the Start menu; optionally it starts when you sign in.
- **Install for all users** (admin): installs to `C:\Program Files\Reelix` and runs Reelix as the Windows service `Reelix`, which starts with Windows even when nobody is signed in. Data lives in `C:\ProgramData\Reelix`.

A new installation then offers a **Fresh install**, a **Recovery** from a Reelix backup zip, or a **Migration** from Radarr, Sonarr, Prowlarr, Lidarr or Readarr on the same PC (reviewed and run in System → Migrate after setup) or from an existing Reelix folder such as the portable zip; silent installs take `/MODE=`, `/BACKUP=`, `/MIGRATEFROM=` and `/ARR=`.

Either way, **Open Reelix** in the Start menu opens <http://localhost:7700>, updates install in place from **System → Updates**, and uninstalling keeps your data unless you ask to delete it. See [docs/installers.md](docs/installers.md).

**Portable zip:**

1. Download `reelix-<version>-windows-x86_64.zip` and unzip it anywhere.
2. Double-click `start-reelix.cmd`.
3. Open <http://localhost:7700>.

Settings and the database are stored in the `data` folder next to `reelix.exe`. Install [FFmpeg](https://ffmpeg.org/download.html) and make sure `ffprobe` is on your `PATH` so Reelix can read codec, resolution and audio details from imported files.

### Linux

Runs on any x86_64 or aarch64 distribution with glibc 2.17 or newer (CentOS 7, Ubuntu 16.04, Debian 9 and later). On Alpine or other musl-based systems, use Docker.

**systemd service (one line):**

```bash
curl -fsSL https://raw.githubusercontent.com/demonzoned/Reelix/master/packaging/linux/install.sh | sudo sh
```

This verifies the download against `SHA256SUMS`, installs to `/opt/reelix` (owned by a `reelix` system user so Reelix can update itself), keeps data in `/var/lib/reelix` and starts the `reelix` service. Run it again to upgrade; pass options after `sh -s --`, e.g. `| sudo sh -s -- --port 8080` or `--version 0.7.0`, and `--uninstall` to remove it (data is kept unless you add `--purge`). Logs: `journalctl -u reelix -f`. Media folders under `/home` need a `ReadWritePaths=` drop-in (`systemctl edit reelix`); see [docs/installers.md](docs/installers.md).

**Tarball:**

```bash
tar xzf reelix-<version>-linux-x86_64.tar.gz
cd reelix-<version>-linux-x86_64
./start-reelix.sh
```

Then open <http://localhost:7700>. Settings and the database are stored in the `data` folder next to the binary (or pass `--data-dir`). Install `ffprobe` with your package manager (e.g. `apt install ffmpeg`).

### Updating

On the Windows and Linux releases (portable archive, Windows installer or service, Linux `install.sh`), System → Updates has an **Install and restart** button. Reelix downloads the release archive from GitHub over HTTPS, checks it against the release's `SHA256SUMS`, backs up the database, replaces its program files (`install.json`, `data/` and everything else are left alone) and restarts. Turn on **Install updates automatically** to have the scheduled check do this for new stable releases. Updates never go to an older version. In Docker, run `docker compose pull && docker compose up -d` instead.

### From source

Requires a stable Rust toolchain.

```bash
rustup target add wasm32-unknown-unknown
cargo install --locked trunk

(cd crates/reelix-web && trunk build --release)
cargo build --release -p reelix-server
./target/release/reelix
```

Run the binary from the repository root so it finds the web UI in `crates/reelix-web/dist`, or point `REELIX_WEB_DIR` at the built UI.

### First run

> **Warning:** don't expose the port to the internet yet. Complete setup before exposing Reelix publicly (port forwarding, reverse proxy or tunnel).

Open <http://localhost:7700>. The setup wizard first asks how you want to start:

- **New install:** the admin account, what to follow (movies, series, books, music), the TMDB key, library folders, a torrent app and a Usenet app, which to prefer, sites to search (indexers), quality, when to get new movies, media servers and requests. Every step after the account can be skipped and changed later in Settings.
- **Restore a Reelix backup:** choose a backup file. It is restored when Reelix restarts, with the accounts and settings in the backup.
- **Migrate from the *arr apps:** the admin account, then Radarr, Sonarr, Prowlarr, Readarr, Lidarr or Jellyseerr by address and API key, with a preview before anything comes over. The rest of the steps follow, for anything the old apps did not have.

Unless you are browsing from the server itself, the wizard asks for the one-time setup token that Reelix prints to its log (and stdout) when it starts without any users, e.g. `docker logs reelix` or `journalctl -u reelix`. For the Windows service it is in `C:\ProgramData\Reelix\logs` (readable from an Administrator PowerShell). A new token is made at every start until the admin account exists.

**No desktop on the server?** Run the same wizard in a terminal: `docker exec -it reelix /app/reelix setup` (or `reelix setup`). Every command, from installing to the last question, is in [Set up Reelix from a terminal only](docs/terminal-setup.md). It asks the same questions and saves them through Reelix's API on the server, so no setup token is needed. Add `--plain` (or set `REELIX_PLAIN=1`) for plain lines that a screen reader can follow, with nothing redrawn.

To ask the questions again later, open **Settings → Set-up Wizard** and choose **Launch wizard**.

**Send feedback** is on every step of the wizard. A report names the step and opens a new GitHub issue; **Show as a QR code** sends it from a phone when the server is not online yet. Nothing typed into the wizard, and no password, key or setup token, is in a report.

If you skipped the TMDB step, add the key under **Settings → Metadata**. Movies and TV need it; books, music and radio don't need any keys (a Podcast Index key is optional).

For radio and podcasts, choose your Audiobookshelf podcast library and its folder under **Settings → Radio**, and add a Radio root folder under **Settings → Media management**.

### Recommended folder layout

Keep downloads and media on the same filesystem, under one mount, so imports can hardlink and move atomically:

```
/data
├── torrents/{movies,tv,books,music}
├── usenet/{movies,tv,books,music}
└── media/{movies,tv,books,audiobooks,music}
```

---

## Configuration

Almost everything is configured in the web UI and stored in the database. A small `config.toml` in the data directory holds the startup options. Reelix writes a default one on first run.

Precedence: **command-line flags > environment variables > `config.toml` > defaults.**

```toml
[server]
bind = "0.0.0.0"
port = 7700
url_base = ""                 # e.g. "/reelix" behind a reverse proxy

[database]
# url = "sqlite:///config/reelix.db"   # defaults to <data_dir>/reelix.db

[paths]
data_dir = "/config"
# backup_dir = "/config/backups"
# log_dir = "/config/logs"
# web_dir = "/app/web"

[security]
# secret_key_file = "/config/secret.key"   # generated on first run
trusted_proxies = []          # e.g. ["172.16.0.0/12"]
secure_cookies = "auto"       # auto (X-Forwarded-Proto: https) | always | never

[logging]
level = "info"                # trace | debug | info | warn | error
format = "pretty"             # pretty | json
```

| Environment variable | Setting |
|---|---|
| `REELIX_PORT`, `REELIX_BIND`, `REELIX_URL_BASE` | Server |
| `REELIX_DATABASE_URL` | Database URL |
| `REELIX_DATA_DIR`, `REELIX_LOG_DIR`, `REELIX_WEB_DIR` | Paths |
| `REELIX_SECRET_KEY_FILE`, `REELIX_SECRET_KEY` | Key used to encrypt stored secrets |
| `REELIX_SECURE_COOKIES` | `Secure` flag on session cookies |
| `REELIX_LOG_LEVEL`, `REELIX_LOG_FORMAT` | Logging |
| `PUID`, `PGID`, `UMASK`, `TZ` | Docker only |

Command-line flags: `--data-dir`, `--config`, `--port` and `--bind`. Run `reelix --help` for details.

### Reverse proxy

Set `url_base` when serving Reelix under a sub-path, and list your proxy's addresses in `trusted_proxies` so `X-Forwarded-For` is used for the client address (login rate limiting and server-local checks). Reelix uses WebSockets for live updates, so the proxy must allow connection upgrades.

---

## Development

```bash
# Backend on :7700 (migrations run on startup, data in ./data)
cargo run -p reelix-server

# Frontend with hot reload on :8080 (proxies /api to :7700)
cd crates/reelix-web && trunk serve

# Tests and lints
cargo test --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p reelix-web --target wasm32-unknown-unknown -- -D warnings
```

Architecture, the domain model and design decisions are described in [docs/design.md](docs/design.md).

---

## License

Reelix is licensed under the [GNU General Public License v3.0 or later](https://www.gnu.org/licenses/gpl-3.0.html). Code adapted from other projects is listed with its license in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

Reelix is a media management and automation tool. It doesn't host, index or distribute content. You are responsible for complying with the laws of your jurisdiction and the terms of the services you connect.

*This product uses the TMDB API but is not endorsed or certified by TMDB.*
