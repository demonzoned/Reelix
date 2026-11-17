//! aria2 adapter (JSON-RPC 2.0 over HTTP, aria2 1.18.4+).
//!
//! aria2 has no categories or labels, so Reelix passes `dir={base}/{category}` with every
//! download and only tracks downloads whose `dir` matches one of its category folders.
//! Download ids are torrent info hashes (a magnet's metadata download and the content download
//! it generates share one), with GIDs resolved on demand for pause/remove/queue actions. The RPC
//! has no way to delete files, so removing a download only clears it from aria2.

use async_trait::async_trait;
use base64::Engine;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use reelix_core::{ContentKind, Protocol};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::common::{self, CommonSettings, HttpSettings, de_opt_f64, de_priority, version_at_least};
use crate::model::*;
use crate::torrent::{self, TorrentSource};
use crate::{BuildContext, ClientError, DownloadClient};

pub const IMPLEMENTATION: &str = "aria2";
/// RPC method-level authorization (`--rpc-secret`) landed in 1.18.4.
pub const MIN_VERSION: &str = "1.18.4";
const FORCE_PRIORITY_UNSUPPORTED: &str =
    "force-starting downloads (aria2 cannot bypass its concurrency limit)";
const FORCE_PRIORITY_MESSAGE: &str =
    "aria2 cannot force-start downloads past its concurrency limit; choose High or lower";
/// `tellWaiting` / `tellStopped` page size.
const PAGE: usize = 1000;
/// Safety cap for pagination (20 000 downloads per list).
const MAX_PAGES: usize = 20;

#[derive(Debug, Clone, Deserialize)]
pub struct Aria2Settings {
    #[serde(flatten)]
    pub common: CommonSettings,
    #[serde(flatten)]
    pub http: HttpSettings,
    /// `--rpc-secret`; empty when the RPC has no token.
    #[serde(default)]
    pub rpc_secret: Option<String>,
    pub movie_category: String,
    pub tv_category: String,
    /// Ebooks and audiobooks; empty = the movie category.
    #[serde(default)]
    pub book_category: String,
    /// Empty = the movie category.
    #[serde(default)]
    pub music_category: String,
    /// Base download folder; empty = aria2's global `dir`.
    #[serde(default)]
    pub directory: String,
    #[serde(deserialize_with = "de_priority")]
    pub recent_priority: Priority,
    #[serde(deserialize_with = "de_priority")]
    pub older_priority: Priority,
    #[serde(default)]
    pub add_paused: bool,
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub seed_ratio: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub seed_time_minutes: Option<f64>,
}

pub fn schema() -> ProviderSchema {
    let mut fields = vec![common::f_name("aria2"), common::f_host(), common::f_port(6800)];
    fields.extend(common::f_ssl());
    fields.push(
        common::f_url_base("").help("Path prefix before /jsonrpc when aria2 runs behind a reverse proxy"),
    );
    fields.push(
        FieldSchema::new("rpc_secret", "RPC secret", FieldKind::Secret)
            .help("Value of aria2's --rpc-secret option. Empty if the RPC has no secret."),
    );
    fields.extend(common::f_categories("movies-reelix", "tv-reelix", "folder"));
    fields.extend(common::f_extra_categories("folder"));
    fields.push(
        FieldSchema::new("directory", "Directory", FieldKind::Path)
            .help(
                "Base download folder; the category is appended as a subfolder. Empty = aria2's global dir.",
            )
            .advanced(),
    );
    fields.extend(priority_fields());
    fields.push(FieldSchema::new("add_paused", "Add paused", FieldKind::Bool).default_value(false));
    fields.push(
        FieldSchema::new("seed_ratio", "Seed ratio", FieldKind::Number)
            .help("Default ratio limit when the indexer sets none (empty = aria2 global setting)"),
    );
    fields.push(FieldSchema::new(
        "seed_time_minutes",
        "Seed time (minutes)",
        FieldKind::Number,
    ));
    fields.extend(common::f_common_tail());
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "aria2".into(),
        description: Some(
            "Torrent client via JSON-RPC (aria2 1.18.4 or newer). Removing a download only \
             clears it from aria2; files are never deleted over RPC."
                .into(),
        ),
        info_url: Some("https://aria2.github.io/manual/en/html/aria2c.html#rpc-interface".into()),
        fields,
    }
}

/// aria2 can move a download to the front of its queue, but cannot force it past the
/// configured concurrency limit. Do not offer Force as a selectable priority.
fn priority_fields() -> [FieldSchema; 2] {
    let mut fields = common::f_recent_older_priority();
    for field in &mut fields {
        if let FieldKind::Select { options } = &mut field.kind {
            options.retain(|option| option.value.as_str() != Some(Priority::Force.as_str()));
        }
    }
    fields
}

/// One entry from `tellActive`, `tellWaiting`, `tellStopped` or `tellStatus`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Aria2Download {
    #[serde(default)]
    pub gid: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, deserialize_with = "de_num_str")]
    pub total_length: u64,
    #[serde(default, deserialize_with = "de_num_str")]
    pub completed_length: u64,
    #[serde(default, deserialize_with = "de_num_str")]
    pub upload_length: u64,
    #[serde(default, deserialize_with = "de_num_str")]
    pub download_speed: u64,
    #[serde(default, deserialize_with = "de_i64_str")]
    pub error_code: i64,
    #[serde(default)]
    pub error_message: String,
    #[serde(default, deserialize_with = "de_bool_str")]
    pub seeder: bool,
    #[serde(default)]
    pub info_hash: Option<String>,
    #[serde(default)]
    pub followed_by: Vec<String>,
    #[serde(default)]
    pub following: Option<String>,
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub files: Vec<Aria2File>,
    #[serde(default)]
    pub bittorrent: Option<Aria2BitTorrent>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Aria2File {
    #[serde(default)]
    pub path: String,
    #[serde(default, deserialize_with = "de_num_str")]
    pub length: u64,
    #[serde(default, deserialize_with = "de_num_str")]
    pub completed_length: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Aria2BitTorrent {
    #[serde(default)]
    pub info: Option<Aria2TorrentInfo>,
    /// `single` or `multi`.
    #[serde(default)]
    pub mode: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Aria2TorrentInfo {
    #[serde(default)]
    pub name: String,
}

/// Number encoded as a JSON string (aria2 encodes all values as strings).
fn de_num_str<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Value::deserialize(d)?;
    match &v {
        Value::Null => Ok(0),
        Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| serde::de::Error::custom(format!("expected a non-negative number, got {v}"))),
        Value::String(s) if s.trim().is_empty() => Ok(0),
        Value::String(s) => s
            .trim()
            .parse()
            .map_err(|_| serde::de::Error::custom(format!("expected a number, got {v}"))),
        _ => Err(serde::de::Error::custom(format!("expected a number, got {v}"))),
    }
}

fn de_i64_str<'de, D>(d: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Value::deserialize(d)?;
    match &v {
        Value::Null => Ok(0),
        Value::Number(n) => n
            .as_i64()
            .ok_or_else(|| serde::de::Error::custom(format!("expected an integer, got {v}"))),
        Value::String(s) if s.trim().is_empty() => Ok(0),
        Value::String(s) => s
            .trim()
            .parse()
            .map_err(|_| serde::de::Error::custom(format!("expected an integer, got {v}"))),
        _ => Err(serde::de::Error::custom(format!("expected an integer, got {v}"))),
    }
}

/// Boolean encoded as `"true"` / `"false"` (also accepts real JSON booleans in tests).
fn de_bool_str<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::Number(n) => n.as_i64().is_some_and(|n| n != 0),
        Value::String(s) => s.trim().eq_ignore_ascii_case("true"),
        _ => false,
    })
}

/// aria2 status → Reelix state.
pub fn map_state(d: &Aria2Download) -> DownloadState {
    use DownloadState::*;
    let complete = d.total_length > 0 && d.completed_length >= d.total_length;
    match d.status.as_str() {
        "active" if d.seeder || complete => Completed,
        "active" => Downloading,
        "waiting" if complete => Completed,
        "waiting" => Queued,
        "paused" if complete => Completed,
        "paused" => Paused,
        "error" => Failed,
        "complete" => Completed,
        _ => Warning,
    }
}

/// Multi-file torrents live in `dir/info.name`; single files are reported by their file path.
fn output_path(d: &Aria2Download) -> Option<String> {
    let info_name = d
        .bittorrent
        .as_ref()
        .and_then(|b| b.info.as_ref())
        .map(|i| i.name.trim())
        .filter(|n| !n.is_empty());
    let first_file = d.files.iter().find(|f| !f.path.trim().is_empty());
    let multi = d.bittorrent.as_ref().is_some_and(|b| b.mode == "multi") || d.files.len() > 1;
    match (info_name, multi, first_file) {
        (Some(name), true, _) => Some(join_path(&d.dir, name)),
        (Some(name), false, None) => Some(join_path(&d.dir, name)),
        (Some(_), false, Some(f)) => Some(f.path.clone()),
        (None, _, Some(f)) => Some(f.path.clone()),
        (None, _, None) => (!d.dir.trim().is_empty()).then(|| d.dir.trim().to_owned()),
    }
}

fn file_name(path: &str) -> Option<String> {
    path.rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn title(d: &Aria2Download) -> String {
    d.bittorrent
        .as_ref()
        .and_then(|b| b.info.as_ref())
        .map(|i| i.name.trim())
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .or_else(|| d.files.iter().find_map(|f| file_name(&f.path)))
        .unwrap_or_else(|| d.gid.clone())
}

/// Download id: the torrent info hash, so a magnet's metadata download and the content
/// download it generates share one id (and the id is stable across aria2 restarts).
fn download_key(d: &Aria2Download) -> String {
    d.info_hash
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| d.gid.clone())
}

pub fn to_item(d: Aria2Download, category: &str) -> DownloadItem {
    let state = map_state(&d);
    let mut item = DownloadItem::new(download_key(&d), title(&d), state);
    item.category = Some(category.to_owned());
    item.total_bytes = d.total_length;
    item.remaining_bytes = d.total_length.saturating_sub(d.completed_length);
    item.eta_secs =
        (d.download_speed > 0 && item.remaining_bytes > 0).then(|| item.remaining_bytes / d.download_speed);
    item.output_path = output_path(&d);
    item.message = (state == DownloadState::Failed).then(|| {
        let m = d.error_message.trim();
        if m.is_empty() {
            format!("aria2 error {}", d.error_code)
        } else {
            m.to_owned()
        }
    });
    item.seed_ratio = (d.total_length > 0).then(|| d.upload_length as f64 / d.total_length as f64);
    // aria2 drops completed results from memory before long, and reports no completion time.
    // Only a download in the `complete` state has stopped seeding.
    let done = d.status == "complete";
    item.can_be_removed = done;
    item.can_move_files = done;
    item
}

/// Combines a base directory and a child, keeping `/`, `C:\` and UNC-style roots intact.
fn join_path(base: &str, name: &str) -> String {
    let base = base.trim();
    if base.is_empty() {
        return name.to_owned();
    }
    if matches!(base, "/" | "\\") || base.ends_with(['/', '\\']) {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// Paths compare after separator normalisation; Windows-style paths are case-insensitive.
fn path_key(p: &str) -> String {
    let windows = p.contains('\\') || p.contains(':');
    let key = p.trim().replace('\\', "/");
    let key = key.trim_end_matches('/').to_owned();
    if windows { key.to_ascii_lowercase() } else { key }
}

fn same_path(a: &str, b: &str) -> bool {
    path_key(a) == path_key(b)
}

fn last_component(path: &str) -> &str {
    path.trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
}

fn is_absolute_path(path: &str) -> bool {
    let path = path.trim();
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with('\\')
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

fn category_of(d: &Aria2Download, base: &str, categories: &[String]) -> Option<String> {
    let dir = d.dir.trim();
    if dir.is_empty() {
        return None;
    }
    let windows = dir.contains('\\') || dir.contains(':');
    categories
        .iter()
        .find(|c| {
            same_path(dir, &join_path(base, c))
                // If aria2's base directory is relative or unavailable, its working directory
                // is unknown to Reelix. In that case only the final component is usable. With
                // an absolute base, do not claim matching folders elsewhere on the filesystem.
                || (!is_absolute_path(base)
                    && !c.contains(['/', '\\'])
                    && if windows {
                        last_component(dir).eq_ignore_ascii_case(c)
                    } else {
                        last_component(dir) == c.as_str()
                    })
        })
        .cloned()
}

fn option_u64(opts: &Value, name: &str) -> Option<u64> {
    match opts.get(name)? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn option_bool(opts: &Value, name: &str) -> Option<bool> {
    match opts.get(name)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => Some(s.trim().eq_ignore_ascii_case("true")),
        Value::Number(n) => Some(n.as_i64().is_some_and(|n| n != 0)),
        _ => None,
    }
}

fn has_bit_torrent(version: &Value) -> bool {
    version
        .get("enabledFeatures")
        .and_then(Value::as_array)
        .is_some_and(|features| features.iter().any(|f| f.as_str() == Some("BitTorrent")))
}

/// aria2 client.
pub struct Aria2 {
    http: reqwest::Client,
    fetch: reqwest::Client,
    url: String,
    settings: Aria2Settings,
    info: ClientInfo,
}

impl Aria2 {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ClientError> {
        let s: Aria2Settings = common::decode_settings(&schema(), settings)?;
        Ok(Self {
            http: s.http.client(ctx)?,
            fetch: torrent::no_redirect_client(),
            url: format!("{}/jsonrpc", s.http.base_url()?),
            info: s.common.info(IMPLEMENTATION),
            settings: s,
        })
    }

    /// One JSON-RPC call. The optional `token:` parameter is prepended here.
    async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value, ClientError> {
        let mut all = Vec::with_capacity(params.len() + 1);
        if let Some(secret) = self.settings.rpc_secret.as_deref().filter(|s| !s.is_empty()) {
            all.push(json!(format!("token:{secret}")));
        }
        all.extend(params);
        let body = json!({
            "jsonrpc": "2.0",
            "id": "reelix",
            "method": method,
            "params": all,
        });
        let text = common::ok_text(self.http.post(&self.url).json(&body).send().await?).await?;
        let response: Value = serde_json::from_str(&text)?;
        if let Some(error) = response.get("error") {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown RPC error");
            if message.to_ascii_lowercase().contains("unauthorized") {
                return Err(ClientError::Auth(message.to_owned()));
            }
            return Err(ClientError::Api(format!("{message} (code {code})")));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn version(&self) -> Result<Value, ClientError> {
        self.call("aria2.getVersion", Vec::new()).await
    }

    fn version_string(version: &Value) -> Result<String, ClientError> {
        version
            .get("version")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| ClientError::Parse("aria2 returned no version".into()))
    }

    async fn base_dir(&self, options: Option<&Value>) -> Result<String, ClientError> {
        let configured = self.settings.directory.trim();
        if !configured.is_empty() {
            return Ok(configured.to_owned());
        }
        let opts = match options {
            Some(o) => o.clone(),
            None => self.call("aria2.getGlobalOption", Vec::new()).await?,
        };
        Ok(opts
            .get("dir")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned())
    }

    fn categories_cfg(&self) -> Vec<String> {
        common::configured_categories_all(&[
            &self.settings.movie_category,
            &self.settings.tv_category,
            &self.settings.book_category,
            &self.settings.music_category,
        ])
    }

    fn force_priority_configured(&self) -> bool {
        self.settings.recent_priority == Priority::Force || self.settings.older_priority == Priority::Force
    }

    async fn paged(&self, method: &str) -> Result<Vec<Aria2Download>, ClientError> {
        let mut out = Vec::new();
        for page in 0..MAX_PAGES {
            let offset = (page * PAGE) as i64;
            let value = self.call(method, vec![json!(offset), json!(PAGE)]).await?;
            let mut rows: Vec<Aria2Download> = serde_json::from_value(value)?;
            let count = rows.len();
            out.append(&mut rows);
            if count < PAGE {
                break;
            }
        }
        Ok(out)
    }

    /// Active + waiting + stopped downloads. A magnet first has a metadata download and then
    /// a new download for the content; both are returned here.
    async fn rows(&self) -> Result<Vec<Aria2Download>, ClientError> {
        let mut rows: Vec<Aria2Download> =
            serde_json::from_value(self.call("aria2.tellActive", Vec::new()).await?)?;
        rows.extend(self.paged("aria2.tellWaiting").await?);
        rows.extend(self.paged("aria2.tellStopped").await?);
        Ok(rows)
    }

    /// Resolves a download id (info hash, or a GID) to the GID used by action methods.
    async fn gid_for(&self, id: &DownloadId) -> Result<String, ClientError> {
        let key = id.as_str().trim();
        // A 16-character hex id is already a GID; info hashes are 40 characters.
        if key.len() == 16 && key.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(key.to_owned());
        }
        let mut fallback = None;
        for row in self.rows().await? {
            if row
                .info_hash
                .as_deref()
                .is_some_and(|h| h.trim().eq_ignore_ascii_case(key))
            {
                // Prefer the content download generated from a magnet over its metadata shell.
                if row.following.is_some() {
                    return Ok(row.gid);
                }
                fallback.get_or_insert(row.gid);
            }
        }
        fallback.ok_or_else(|| ClientError::NotFound(key.to_owned()))
    }

    async fn change_position(&self, gid: &str, pos: i64, how: &str) -> Result<(), ClientError> {
        self.call("aria2.changePosition", vec![json!(gid), json!(pos), json!(how)])
            .await
            .map(|_| ())
    }

    async fn run_checks(&self) -> (TestReport, Option<ClientError>) {
        let mut report = TestReport::default();
        let version = match self.version().await {
            Ok(v) => {
                report.pass("connect");
                report.pass("auth");
                v
            }
            Err(e @ ClientError::Auth(_)) => {
                report.pass("connect");
                report.fail("auth", e.to_string());
                return (report, Some(e));
            }
            Err(e) => {
                report.fail("connect", e.to_string());
                return (report, Some(e));
            }
        };
        let found = match Self::version_string(&version) {
            Ok(v) => v,
            Err(e) => {
                report.fail("version", e.to_string());
                return (report, Some(e));
            }
        };
        if !version_at_least(&found, MIN_VERSION) {
            let e = ClientError::UnsupportedVersion {
                found,
                minimum: MIN_VERSION.into(),
            };
            report.fail("version", e.to_string());
            return (report, Some(e));
        }
        report.pass("version");
        if !has_bit_torrent(&version) {
            let m = "aria2 was built without BitTorrent support";
            report.fail("bittorrent", m);
            return (report, Some(ClientError::InvalidSettings(m.into())));
        }
        report.pass("bittorrent");
        let options = match self.call("aria2.getGlobalOption", Vec::new()).await {
            Ok(o) => o,
            Err(e) => {
                report.fail("options", e.to_string());
                return (report, Some(e));
            }
        };
        if option_u64(&options, "max-download-result") == Some(0) {
            let m = "aria2 has max-download-result=0, so finished downloads disappear before Reelix can import them; set it above 0";
            report.fail("history", m);
            return (report, Some(ClientError::InvalidSettings(m.into())));
        }
        report.pass("history");
        if option_bool(&options, "bt-metadata-only") == Some(true) {
            let m = "aria2 has bt-metadata-only=true, so magnet links download metadata but never the files";
            report.fail("metadata", m);
            return (report, Some(ClientError::InvalidSettings(m.into())));
        }
        report.pass("metadata");
        if self.force_priority_configured() {
            report.fail("priority", FORCE_PRIORITY_MESSAGE);
            return (
                report,
                Some(ClientError::InvalidSettings(FORCE_PRIORITY_MESSAGE.into())),
            );
        }
        report.pass("priority");
        (report, None)
    }
}

#[async_trait]
impl DownloadClient for Aria2 {
    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }

    fn info(&self) -> &ClientInfo {
        &self.info
    }

    fn category_for_kind(&self, kind: ContentKind) -> Option<String> {
        let s = &self.settings;
        common::category_for_kind(
            kind,
            &s.movie_category,
            &s.tv_category,
            &s.book_category,
            &s.music_category,
        )
    }

    async fn add(&self, release: &ReleasePayload, opts: &AddOptions) -> Result<DownloadId, ClientError> {
        let prio = opts.priority.unwrap_or(if opts.is_recent {
            self.settings.recent_priority
        } else {
            self.settings.older_priority
        });
        if prio == Priority::Force {
            return Err(ClientError::Unsupported(FORCE_PRIORITY_UNSUPPORTED));
        }
        let source = torrent::resolve(&self.fetch, release, &opts.title).await?;
        let category = common::category_for(
            opts.category.as_deref(),
            opts.media_kind,
            &self.settings.movie_category,
            &self.settings.tv_category,
        );
        let paused = opts.add_paused || self.settings.add_paused || prio == Priority::Paused;

        let mut options = serde_json::Map::new();
        if let Some(category) = &category {
            let base = self.base_dir(None).await?;
            options.insert("dir".into(), json!(join_path(&base, category)));
        }
        if paused {
            options.insert("pause".into(), json!("true"));
        }
        if let Some(ratio) = opts.seed_ratio.or(self.settings.seed_ratio).filter(|r| *r > 0.0) {
            options.insert("seed-ratio".into(), json!(ratio.to_string()));
        }
        if let Some(minutes) = opts
            .seed_time_minutes
            .map(|m| m as f64)
            .or(self.settings.seed_time_minutes)
            .filter(|m| *m > 0.0)
        {
            options.insert("seed-time".into(), json!(minutes.to_string()));
        }

        // Insert high-priority jobs at the front of the waiting queue; low priority is moved
        // to the end after aria2 assigned the GID.
        let position = (prio == Priority::High).then_some(0i64);
        let result = match &source {
            TorrentSource::Magnet { uri, .. } => {
                let mut params = vec![json!([uri]), Value::Object(options)];
                if let Some(pos) = position {
                    params.push(json!(pos));
                }
                self.call("aria2.addUri", params).await
            }
            TorrentSource::File { bytes, .. } => {
                let torrent = base64::engine::general_purpose::STANDARD.encode(bytes);
                let mut params = vec![json!(torrent), json!([]), Value::Object(options)];
                if let Some(pos) = position {
                    params.push(json!(pos));
                }
                self.call("aria2.addTorrent", params).await
            }
        };
        let result = match result {
            Ok(v) => v,
            Err(ClientError::Api(message)) => return Err(ClientError::Rejected(message)),
            Err(e) => return Err(e),
        };
        let gid = match result {
            Value::String(gid) if !gid.trim().is_empty() => gid,
            _ => return Err(ClientError::Rejected("aria2 returned no GID".into())),
        };
        if prio == Priority::Low {
            // Best effort; the queue may have been changed meanwhile.
            let _ = self.change_position(&gid, 0, "POS_END").await;
        }
        // Track by info hash: a magnet's metadata download is followed by a *new* download
        // for the content, and both carry the same info hash.
        Ok(DownloadId(source.hash().to_owned()))
    }

    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError> {
        let categories = self.categories_cfg();
        if categories.is_empty() {
            return Ok(Vec::new());
        }
        let base = self.base_dir(None).await?;

        let mut rows = self.rows().await?;
        // A magnet can briefly have both the metadata shell and the content download. Prefer
        // the content download (it has `following` set).
        rows.sort_by_key(|r| r.following.is_none());

        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for row in rows {
            if row.gid.is_empty() || row.status == "removed" || !row.followed_by.is_empty() {
                continue;
            }
            if !seen.insert(download_key(&row)) {
                continue;
            }
            if let Some(category) = category_of(&row, &base, &categories) {
                out.push(to_item(row, &category));
            }
        }
        Ok(out)
    }

    async fn remove(&self, id: &DownloadId, _delete_data: bool) -> Result<(), ClientError> {
        let gid = match self.gid_for(id).await {
            Ok(gid) => gid,
            Err(ClientError::NotFound(_)) => return Ok(()),
            Err(e) => return Err(e),
        };
        let gid = json!(gid);
        // Active/waiting downloads are stopped by forceRemove; stopped results are cleared by
        // removeDownloadResult. Either may be missing, which is fine.
        match self.call("aria2.forceRemove", vec![gid.clone()]).await {
            Ok(_) | Err(ClientError::Api(_)) => {}
            Err(e) => return Err(e),
        }
        match self.call("aria2.removeDownloadResult", vec![gid]).await {
            Ok(_) | Err(ClientError::Api(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn test(&self) -> Result<(), ClientError> {
        self.run_checks().await.1.map_or(Ok(()), Err)
    }

    async fn test_report(&self) -> TestReport {
        self.run_checks().await.0
    }

    async fn status(&self) -> Result<ClientStatus, ClientError> {
        let version = self.version().await?;
        let found = Self::version_string(&version)?;
        let options = self.call("aria2.getGlobalOption", Vec::new()).await?;
        let mut warnings = Vec::new();
        if !version_at_least(&found, MIN_VERSION) {
            warnings.push(HealthWarning::new(
                HealthKind::VersionTooOld,
                format!("aria2 {found} is too old; Reelix needs {MIN_VERSION} or newer"),
            ));
        }
        if !has_bit_torrent(&version) {
            warnings.push(HealthWarning::new(
                HealthKind::Misconfigured,
                "aria2 was built without BitTorrent support",
            ));
        }
        if option_u64(&options, "max-download-result") == Some(0) {
            warnings.push(
                HealthWarning::new(
                    HealthKind::HistoryRetention,
                    "aria2 has max-download-result=0, so completed downloads vanish before Reelix can import them",
                )
                .at("/#max-download-result"),
            );
        }
        if option_bool(&options, "bt-metadata-only") == Some(true) {
            warnings.push(HealthWarning::new(
                HealthKind::Misconfigured,
                "aria2 has bt-metadata-only=true, so magnet links never download the files",
            ));
        }
        if self.force_priority_configured() {
            warnings.push(HealthWarning::new(
                HealthKind::Misconfigured,
                FORCE_PRIORITY_MESSAGE,
            ));
        }
        let base = self.base_dir(Some(&options)).await?;
        let mut output_dirs = Vec::new();
        if !base.trim().is_empty() {
            output_dirs.push(OutputDir {
                category: None,
                path: base.clone(),
            });
            for category in self.categories_cfg() {
                output_dirs.push(OutputDir {
                    path: join_path(&base, &category),
                    category: Some(category),
                });
            }
        }
        Ok(ClientStatus {
            version: Some(found),
            output_dirs,
            warnings,
        })
    }

    async fn categories(&self) -> Result<Vec<String>, ClientError> {
        // aria2 has no categories; Reelix's category folders are the only ones it tracks.
        Ok(self.categories_cfg())
    }

    async fn set_priority(&self, id: &DownloadId, priority: Priority) -> Result<(), ClientError> {
        if matches!(priority, Priority::Default | Priority::Normal) {
            return Ok(());
        }
        if priority == Priority::Force {
            return Err(ClientError::Unsupported(FORCE_PRIORITY_UNSUPPORTED));
        }
        let gid = self.gid_for(id).await?;
        match priority {
            Priority::High => self.change_position(&gid, 0, "POS_SET").await,
            Priority::Low => self.change_position(&gid, 0, "POS_END").await,
            Priority::Paused => self.call("aria2.pause", vec![json!(gid)]).await.map(|_| ()),
            Priority::Default | Priority::Normal | Priority::Force => Ok(()),
        }
    }

    async fn pause(&self, id: &DownloadId) -> Result<(), ClientError> {
        let gid = self.gid_for(id).await?;
        self.call("aria2.pause", vec![json!(gid)]).await.map(|_| ())
    }

    async fn resume(&self, id: &DownloadId) -> Result<(), ClientError> {
        let gid = self.gid_for(id).await?;
        self.call("aria2.unpause", vec![json!(gid)]).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn download(status: &str, total: u64, done: u64) -> Aria2Download {
        Aria2Download {
            gid: "2089b05ecca3d829".into(),
            status: status.into(),
            total_length: total,
            completed_length: done,
            ..Default::default()
        }
    }

    #[test]
    fn state_table() {
        use DownloadState::*;
        for (status, seeder, total, done, want) in [
            ("active", false, 100, 40, Downloading),
            ("active", false, 100, 100, Completed),
            ("active", true, 100, 100, Completed),
            ("waiting", false, 100, 0, Queued),
            ("waiting", false, 100, 100, Completed),
            ("paused", false, 100, 40, Paused),
            ("paused", false, 100, 100, Completed),
            ("error", false, 100, 40, Failed),
            ("complete", false, 100, 100, Completed),
            ("removed", false, 100, 100, Warning),
            ("nonsense", false, 0, 0, Warning),
        ] {
            let mut d = download(status, total, done);
            d.seeder = seeder;
            assert_eq!(map_state(&d), want, "{status}/{seeder}");
        }
    }

    #[test]
    fn output_paths_and_titles() {
        let mut d = download("active", 100, 50);
        d.dir = "/downloads/tv-reelix".into();
        d.files = vec![Aria2File {
            path: "/downloads/tv-reelix/Show.S01E01.mkv".into(),
            length: 100,
            completed_length: 50,
        }];
        assert_eq!(
            output_path(&d).as_deref(),
            Some("/downloads/tv-reelix/Show.S01E01.mkv")
        );
        assert_eq!(title(&d), "Show.S01E01.mkv");

        d.bittorrent = Some(Aria2BitTorrent {
            info: Some(Aria2TorrentInfo {
                name: "Show.S01".into(),
            }),
            mode: "multi".into(),
        });
        d.files.push(Aria2File {
            path: "/downloads/tv-reelix/Show.S01/E01.mkv".into(),
            length: 100,
            completed_length: 50,
        });
        assert_eq!(output_path(&d).as_deref(), Some("/downloads/tv-reelix/Show.S01"));
        assert_eq!(title(&d), "Show.S01");
    }

    #[test]
    fn category_folders_and_windows_paths() {
        let mut d = download("active", 100, 50);
        d.dir = r"C:\Downloads\TV-Reelix\".into();
        let cats = vec!["movies-reelix".to_owned(), "tv-reelix".to_owned()];
        assert_eq!(
            category_of(&d, r"c:\downloads", &cats).as_deref(),
            Some("tv-reelix")
        );
        assert_eq!(category_of(&d, "/downloads", &cats), None);
        d.dir = "/downloads/other".into();
        assert_eq!(category_of(&d, "/downloads", &cats), None);
        d.dir = "/other/tv-reelix".into();
        assert_eq!(category_of(&d, "/downloads", &cats), None);
        assert_eq!(category_of(&d, ".", &cats).as_deref(), Some("tv-reelix"));
        assert!(is_absolute_path("/downloads"));
        assert!(is_absolute_path(r"C:\Downloads"));
        assert!(!is_absolute_path("./downloads"));
        assert_eq!(join_path("/", "tv"), "/tv");
        assert_eq!(join_path("/downloads/", "tv"), "/downloads/tv");
        assert_eq!(join_path(r"C:\", "tv"), r"C:\tv");
    }

    #[test]
    fn deserialises_string_encoded_rpc_values() {
        let v = json!({
            "gid": "abc",
            "status": "complete",
            "totalLength": "100",
            "completedLength": "100",
            "uploadLength": "10",
            "downloadSpeed": "0",
            "errorCode": "0",
            "seeder": "false",
            "infoHash": "C12FE1C06BBA254A9DC9F519B335AA7C1367A88A",
            "following": "9999999999999999",
            "dir": "/d",
            "files": [{"path": "/d/a.mkv", "length": "100", "completedLength": "100"}],
        });
        let d: Aria2Download = serde_json::from_value(v).unwrap();
        assert_eq!(d.total_length, 100);
        assert!(!d.seeder);
        assert_eq!(d.files[0].length, 100);
        assert_eq!(download_key(&d), "c12fe1c06bba254a9dc9f519b335aa7c1367a88a");
        assert_eq!(d.following.as_deref(), Some("9999999999999999"));
    }
}
