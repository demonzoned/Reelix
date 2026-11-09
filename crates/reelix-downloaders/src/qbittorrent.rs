//! qBittorrent adapter (WebUI API v2, qBittorrent 4.1+ / 5.x).
//!
//! Logs in with `auth/login` and keeps the `SID` cookie itself (re-login on 403), so the
//! shared HTTP client needs no cookie store. Download ids are lowercase info hashes.

use async_trait::async_trait;
use reelix_core::provider::{FieldKind, FieldSchema, ProviderSchema};
use reelix_core::{ContentKind, Protocol};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::common::{self, CommonSettings, HttpSettings, de_opt_f64, de_priority, snippet, version_at_least};
use crate::model::*;
use crate::torrent::{self, TorrentSource};
use crate::{BuildContext, ClientError, DownloadClient};

pub const IMPLEMENTATION: &str = "qbittorrent";
/// Web API 2.0 shipped with qBittorrent 4.1.
pub const MIN_API_VERSION: &str = "2.0";

#[derive(Debug, Clone, Deserialize)]
pub struct QBittorrentSettings {
    #[serde(flatten)]
    pub common: CommonSettings,
    #[serde(flatten)]
    pub http: HttpSettings,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    pub movie_category: String,
    pub tv_category: String,
    /// Ebooks and audiobooks; empty = the movie category.
    #[serde(default)]
    pub book_category: String,
    /// Empty = the movie category.
    #[serde(default)]
    pub music_category: String,
    /// Extra tags applied to every torrent Reelix adds.
    #[serde(default)]
    pub torrent_tags: Vec<String>,
    #[serde(deserialize_with = "de_priority")]
    pub recent_priority: Priority,
    #[serde(deserialize_with = "de_priority")]
    pub older_priority: Priority,
    #[serde(default)]
    pub add_paused: bool,
    #[serde(default)]
    pub sequential: bool,
    #[serde(default)]
    pub first_last_piece: bool,
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub seed_ratio: Option<f64>,
    #[serde(default, deserialize_with = "de_opt_f64")]
    pub seed_time_minutes: Option<f64>,
}

pub fn schema() -> ProviderSchema {
    let mut fields = vec![
        common::f_name("qBittorrent"),
        common::f_host(),
        common::f_port(8080),
    ];
    fields.extend(common::f_ssl());
    fields.push(common::f_url_base(""));
    fields.push(common::f_username(None));
    fields.push(common::f_password(None));
    fields.extend(common::f_categories("movies-reelix", "tv-reelix", "category"));
    fields.extend(common::f_extra_categories("category"));
    fields.push(
        FieldSchema::new("torrent_tags", "Tags on torrents", FieldKind::List)
            .help("qBittorrent tags added to every torrent (comma-separated)")
            .advanced(),
    );
    fields.extend(common::f_recent_older_priority());
    fields.push(FieldSchema::new("add_paused", "Add paused", FieldKind::Bool).default_value(false));
    fields.push(
        FieldSchema::new("sequential", "Sequential download", FieldKind::Bool)
            .default_value(false)
            .advanced(),
    );
    fields.push(
        FieldSchema::new("first_last_piece", "First and last pieces first", FieldKind::Bool)
            .default_value(false)
            .advanced(),
    );
    fields.push(
        FieldSchema::new("seed_ratio", "Seed ratio", FieldKind::Number)
            .help("Default ratio limit when the indexer sets none (empty = qBittorrent global setting)"),
    );
    fields.push(FieldSchema::new(
        "seed_time_minutes",
        "Seed time (minutes)",
        FieldKind::Number,
    ));
    fields.extend(common::f_common_tail());
    ProviderSchema {
        implementation: IMPLEMENTATION.into(),
        name: "qBittorrent".into(),
        description: Some("Torrent client (WebUI API v2, qBittorrent 4.1 or newer)".into()),
        info_url: Some("https://github.com/qbittorrent/qBittorrent/wiki".into()),
        fields,
    }
}

/// `torrents/info` entry (fields we use).
#[derive(Debug, Clone, Deserialize)]
pub struct QbTorrent {
    pub hash: String,
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub amount_left: i64,
    #[serde(default)]
    pub eta: i64,
    #[serde(default)]
    pub progress: f64,
    #[serde(default)]
    pub save_path: String,
    #[serde(default)]
    pub content_path: Option<String>,
    #[serde(default)]
    pub ratio: f64,
    /// -2 = global, -1 = unlimited.
    #[serde(default = "minus_two")]
    pub ratio_limit: f64,
    /// Minutes; -2 = global, -1 = unlimited.
    #[serde(default = "minus_two_i")]
    pub seeding_time_limit: i64,
    /// Seconds.
    #[serde(default)]
    pub seeding_time: Option<i64>,
    /// Unix seconds; -1 or 0 until complete.
    #[serde(default)]
    pub completion_on: i64,
}

fn minus_two() -> f64 {
    -2.0
}
fn minus_two_i() -> i64 {
    -2
}

/// Global seeding limits from `app/preferences`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct QbPreferences {
    #[serde(default)]
    pub save_path: String,
    #[serde(default)]
    pub max_ratio_enabled: bool,
    #[serde(default)]
    pub max_ratio: f64,
    #[serde(default)]
    pub max_seeding_time_enabled: bool,
    /// Minutes.
    #[serde(default)]
    pub max_seeding_time: i64,
    /// 0 = pause/stop, 1 = remove, 2 = enable super seeding, 3 = remove with files.
    #[serde(default)]
    pub max_ratio_act: i64,
    #[serde(default)]
    pub queueing_enabled: bool,
}

/// qBittorrent `state` → Reelix state (+ message).
pub fn map_state(state: &str) -> (DownloadState, Option<&'static str>) {
    use DownloadState::*;
    match state {
        "error" => (Warning, Some("qBittorrent reports an error")),
        "missingFiles" => (Warning, Some("The download is missing files")),
        "pausedDL" | "stoppedDL" => (Paused, None),
        "queuedDL" | "checkingDL" | "checkingUP" | "checkingResumeData" | "allocating" => (Queued, None),
        "metaDL" | "forcedMetaDL" => (Queued, Some("Downloading metadata")),
        "stalledDL" => (Warning, Some("The download is stalled with no connections")),
        "downloading" | "forcedDL" => (Downloading, None),
        "uploading" | "stalledUP" | "queuedUP" | "forcedUP" | "pausedUP" | "stoppedUP" => (Completed, None),
        "moving" => (PostProcessing, Some("Moving files")),
        _ => (Warning, Some("Unknown qBittorrent state")),
    }
}

/// Converts a torrent, deciding removability from torrent and global seed limits.
pub fn to_item(t: QbTorrent, prefs: &QbPreferences) -> DownloadItem {
    let (state, msg) = map_state(&t.state);
    let mut item = DownloadItem::new(t.hash.to_ascii_lowercase(), t.name.clone(), state);
    item.category = Some(t.category.clone()).filter(|c| !c.is_empty());
    item.completed_at = (t.completion_on > 0).then_some(t.completion_on);
    item.total_bytes = t.size.max(0) as u64;
    item.remaining_bytes = t.amount_left.max(0) as u64;
    // 8640000 = qBittorrent's "infinity".
    item.eta_secs = (t.eta > 0 && t.eta < 8_640_000).then_some(t.eta as u64);
    item.message = msg.map(str::to_owned);
    item.seed_ratio = Some(t.ratio);
    item.seeding_time = t.seeding_time.map(|s| s.max(0) as u64);
    item.output_path = Some(
        t.content_path
            .clone()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| format!("{}/{}", t.save_path.trim_end_matches(['/', '\\']), t.name)),
    );
    let stopped = matches!(t.state.as_str(), "pausedUP" | "stoppedUP");
    let done = stopped && seed_goal_reached(&t, prefs);
    item.can_be_removed = done;
    item.can_move_files = done;
    item
}

fn seed_goal_reached(t: &QbTorrent, prefs: &QbPreferences) -> bool {
    let ratio_limit = match t.ratio_limit {
        l if l >= 0.0 => Some(l),
        l if l == -2.0 && prefs.max_ratio_enabled => Some(prefs.max_ratio),
        _ => None,
    };
    let time_limit_min = match t.seeding_time_limit {
        l if l >= 0 => Some(l),
        -2 if prefs.max_seeding_time_enabled => Some(prefs.max_seeding_time),
        _ => None,
    };
    let ratio_hit = ratio_limit.is_some_and(|l| t.ratio >= l - 0.001);
    let time_hit = time_limit_min
        .zip(t.seeding_time)
        .is_some_and(|(l, s)| s >= l * 60);
    ratio_hit || time_hit
}

/// qBittorrent client.
pub struct QBittorrent {
    http: reqwest::Client,
    fetch: reqwest::Client,
    base: String,
    settings: QBittorrentSettings,
    info: ClientInfo,
    sid: Mutex<Option<String>>,
    api_version: Mutex<Option<String>>,
}

impl QBittorrent {
    pub fn from_settings(settings: &Value, ctx: &BuildContext) -> Result<Self, ClientError> {
        let s: QBittorrentSettings = common::decode_settings(&schema(), settings)?;
        Ok(Self {
            http: s.http.client(ctx)?,
            fetch: torrent::no_redirect_client(),
            base: s.http.base_url()?,
            info: s.common.info(IMPLEMENTATION),
            settings: s,
            sid: Mutex::new(None),
            api_version: Mutex::new(None),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v2/{path}", self.base)
    }

    async fn login(&self) -> Result<(), ClientError> {
        let user = self.settings.username.clone().unwrap_or_default();
        let pass = self.settings.password.clone().unwrap_or_default();
        let resp = self
            .http
            .post(self.url("auth/login"))
            // qBittorrent's CSRF check wants a Referer matching the host.
            .header(reqwest::header::REFERER, &self.base)
            .form(&[("username", user), ("password", pass)])
            .send()
            .await?;
        let status = resp.status();
        let cookies: Vec<String> = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|c| c.split(';').next())
            .map(|kv| kv.trim().to_owned())
            .filter(|kv| kv.contains('='))
            .collect();
        let cookie = session_cookie(&cookies);
        let body = common::read_text(resp).await.unwrap_or_default();
        if status.as_u16() == 403 {
            return Err(ClientError::Auth("IP banned after too many failed logins".into()));
        }
        if !status.is_success() {
            return Err(ClientError::Http {
                status: status.as_u16(),
                body: snippet(&body),
            });
        }
        if body.trim() == "Fails." {
            return Err(ClientError::Auth("invalid username or password".into()));
        }
        *self.sid.lock().await = cookie;
        Ok(())
    }

    /// Sends a request with the session cookie; logs in on demand and retries once on 403.
    async fn send(
        &self,
        build: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    ) -> Result<String, ClientError> {
        for attempt in 0..2 {
            if self.sid.lock().await.is_none() && attempt == 0 {
                self.login().await?;
            }
            let mut rb = build(&self.http).header(reqwest::header::REFERER, &self.base);
            if let Some(sid) = self.sid.lock().await.clone() {
                rb = rb.header(reqwest::header::COOKIE, sid);
            }
            let resp = rb.send().await?;
            let status = resp.status();
            if status.as_u16() == 403 && attempt == 0 {
                self.login().await?;
                continue;
            }
            let body = common::read_text(resp).await?;
            if status.as_u16() == 403 {
                return Err(ClientError::Auth("session rejected".into()));
            }
            if status.as_u16() == 404 {
                return Err(ClientError::NotFound(snippet(&body)));
            }
            if !status.is_success() {
                return Err(ClientError::Http {
                    status: status.as_u16(),
                    body: snippet(&body),
                });
            }
            return Ok(body);
        }
        Err(ClientError::Auth("session rejected".into()))
    }

    async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<String, ClientError> {
        let url = self.url(path);
        self.send(|c| c.get(&url).query(query)).await
    }

    async fn post(&self, path: &str, form: &[(&str, &str)]) -> Result<String, ClientError> {
        let url = self.url(path);
        self.send(|c| c.post(&url).form(form)).await
    }

    async fn api_version(&self) -> Result<String, ClientError> {
        if let Some(v) = self.api_version.lock().await.clone() {
            return Ok(v);
        }
        let v = self.get("app/webapiVersion", &[]).await?.trim().to_owned();
        *self.api_version.lock().await = Some(v.clone());
        Ok(v)
    }

    /// qBittorrent 5 (Web API 2.11) renamed pause/resume to stop/start.
    async fn uses_stop_start(&self) -> bool {
        self.api_version()
            .await
            .is_ok_and(|v| version_at_least(&v, "2.11"))
    }

    async fn preferences(&self) -> Result<QbPreferences, ClientError> {
        Ok(serde_json::from_str(&self.get("app/preferences", &[]).await?)?)
    }

    async fn category_map(&self) -> Result<serde_json::Map<String, Value>, ClientError> {
        let v: Value = serde_json::from_str(&self.get("torrents/categories", &[]).await?)?;
        Ok(v.as_object().cloned().unwrap_or_default())
    }

    fn categories_cfg(&self) -> Vec<String> {
        common::configured_categories_all(&[
            &self.settings.movie_category,
            &self.settings.tv_category,
            &self.settings.book_category,
            &self.settings.music_category,
        ])
    }

    async fn ensure_category(&self, cat: &str) -> Result<(), ClientError> {
        if !self.category_map().await?.contains_key(cat) {
            self.post("torrents/createCategory", &[("category", cat), ("savePath", "")])
                .await?;
        }
        Ok(())
    }

    async fn run_checks(&self) -> (TestReport, Option<ClientError>) {
        let mut r = TestReport::default();
        if let Err(e) = self.login().await {
            match e {
                ClientError::Auth(_) => {
                    r.pass("connect");
                    r.fail("auth", e.to_string());
                }
                _ => r.fail("connect", e.to_string()),
            }
            return (r, Some(e));
        }
        r.pass("connect");
        r.pass("auth");
        match self.api_version().await {
            Ok(v) if version_at_least(&v, MIN_API_VERSION) => r.pass("version"),
            Ok(v) => {
                let e = ClientError::UnsupportedVersion {
                    found: v,
                    minimum: MIN_API_VERSION.into(),
                };
                r.fail("version", e.to_string());
                return (r, Some(e));
            }
            Err(e) => {
                r.fail("version", e.to_string());
                return (r, Some(e));
            }
        }
        match self.preferences().await {
            Ok(p) if p.max_ratio_enabled && matches!(p.max_ratio_act, 1 | 3) => {
                let m = "qBittorrent removes torrents when they reach the seed ratio; set the action to Pause/Stop".to_owned();
                r.fail("seeding", m.clone());
                return (r, Some(ClientError::InvalidSettings(m)));
            }
            Ok(_) => r.pass("seeding"),
            Err(e) => {
                r.fail("seeding", e.to_string());
                return (r, Some(e));
            }
        }
        (r, None)
    }
}

#[async_trait]
impl DownloadClient for QBittorrent {
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
        let source = torrent::resolve(&self.fetch, release, &opts.title).await?;
        let category = common::category_for(
            opts.category.as_deref(),
            opts.media_kind,
            &self.settings.movie_category,
            &self.settings.tv_category,
        );
        if let Some(c) = &category {
            self.ensure_category(c).await?;
        }
        let prio = opts.priority.unwrap_or(if opts.is_recent {
            self.settings.recent_priority
        } else {
            self.settings.older_priority
        });
        let paused = opts.add_paused || self.settings.add_paused || prio == Priority::Paused;
        let ratio = opts.seed_ratio.or(self.settings.seed_ratio);
        let seed_time = opts
            .seed_time_minutes
            .map(|m| m as f64)
            .or(self.settings.seed_time_minutes);
        let stop_start = self.uses_stop_start().await;

        let mut fields: Vec<(&'static str, String)> = Vec::new();
        if let Some(c) = &category {
            fields.push(("category", c.clone()));
        }
        if !self.settings.torrent_tags.is_empty() {
            fields.push(("tags", self.settings.torrent_tags.join(",")));
        }
        if paused {
            fields.push((if stop_start { "stopped" } else { "paused" }, "true".into()));
        }
        if let Some(r) = ratio {
            fields.push(("ratioLimit", r.to_string()));
        }
        if let Some(t) = seed_time {
            fields.push(("seedingTimeLimit", (t.round() as i64).to_string()));
        }
        if self.settings.sequential {
            fields.push(("sequentialDownload", "true".into()));
        }
        if self.settings.first_last_piece {
            fields.push(("firstLastPiecePrio", "true".into()));
        }
        let url = self.url("torrents/add");
        let body = self
            .send(|c| {
                let mut form = reqwest::multipart::Form::new();
                for (k, v) in &fields {
                    form = form.text(*k, v.clone());
                }
                form = match &source {
                    TorrentSource::Magnet { uri, .. } => form.text("urls", uri.clone()),
                    TorrentSource::File { bytes, name, .. } => form.part(
                        "torrents",
                        reqwest::multipart::Part::bytes(bytes.clone())
                            .file_name(name.clone())
                            .mime_str("application/x-bittorrent")
                            .unwrap_or_else(|_| reqwest::multipart::Part::bytes(bytes.clone())),
                    ),
                };
                c.post(&url).multipart(form)
            })
            .await?;
        if body.trim() == "Fails." {
            return Err(ClientError::Rejected(
                "qBittorrent refused the torrent (duplicate or invalid)".into(),
            ));
        }
        let hash = source.hash().to_owned();
        if prio == Priority::Force || prio == Priority::High {
            // Best effort: needs the torrent to be registered and queueing enabled.
            let _ = self.set_priority(&DownloadId(hash.clone()), prio).await;
        }
        Ok(DownloadId(hash))
    }

    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError> {
        let prefs = self.preferences().await?;
        let mut out = Vec::new();
        for cat in self.categories_cfg() {
            let body = self.get("torrents/info", &[("category", &cat)]).await?;
            let list: Vec<QbTorrent> = serde_json::from_str(&body)?;
            out.extend(list.into_iter().map(|t| to_item(t, &prefs)));
        }
        Ok(out)
    }

    async fn remove(&self, id: &DownloadId, delete_data: bool) -> Result<(), ClientError> {
        let del = if delete_data { "true" } else { "false" };
        self.post(
            "torrents/delete",
            &[("hashes", id.as_str()), ("deleteFiles", del)],
        )
        .await?;
        Ok(())
    }

    async fn test(&self) -> Result<(), ClientError> {
        self.run_checks().await.1.map_or(Ok(()), Err)
    }

    async fn test_report(&self) -> TestReport {
        self.run_checks().await.0
    }

    async fn status(&self) -> Result<ClientStatus, ClientError> {
        let version = self.get("app/version", &[]).await?.trim().to_owned();
        let prefs = self.preferences().await?;
        let cats = self.category_map().await?;
        let mut warnings = Vec::new();
        if prefs.max_ratio_enabled && matches!(prefs.max_ratio_act, 1 | 3) {
            warnings.push(
                HealthWarning::new(
                    HealthKind::RemovesOnRatio,
                    "qBittorrent removes torrents that reach the seed ratio, so Reelix may never import them",
                )
                .at("/#preferences-bittorrent"),
            );
        }
        let save = prefs.save_path.trim_end_matches(['/', '\\']).to_owned();
        let mut output_dirs = vec![OutputDir {
            category: None,
            path: save.clone(),
        }];
        for c in self.categories_cfg() {
            let path = cats
                .get(&c)
                .and_then(|v| v.get("savePath"))
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
                .map_or_else(
                    || format!("{save}/{c}"),
                    |p| p.trim_end_matches(['/', '\\']).to_owned(),
                );
            output_dirs.push(OutputDir {
                category: Some(c),
                path,
            });
        }
        Ok(ClientStatus {
            version: Some(version),
            output_dirs,
            warnings,
        })
    }

    async fn categories(&self) -> Result<Vec<String>, ClientError> {
        Ok(self.category_map().await?.keys().cloned().collect())
    }

    async fn set_priority(&self, id: &DownloadId, priority: Priority) -> Result<(), ClientError> {
        let h = id.as_str();
        match priority {
            Priority::Force => {
                self.post("torrents/setForceStart", &[("hashes", h), ("value", "true")])
                    .await?;
            }
            Priority::High => {
                self.post("torrents/topPrio", &[("hashes", h)]).await?;
            }
            Priority::Low => {
                self.post("torrents/bottomPrio", &[("hashes", h)]).await?;
            }
            Priority::Paused => return self.pause(id).await,
            Priority::Default | Priority::Normal => {
                self.post("torrents/setForceStart", &[("hashes", h), ("value", "false")])
                    .await?;
            }
        }
        Ok(())
    }

    async fn pause(&self, id: &DownloadId) -> Result<(), ClientError> {
        let path = if self.uses_stop_start().await {
            "torrents/stop"
        } else {
            "torrents/pause"
        };
        self.post(path, &[("hashes", id.as_str())]).await.map(|_| ())
    }

    async fn resume(&self, id: &DownloadId) -> Result<(), ClientError> {
        let path = if self.uses_stop_start().await {
            "torrents/start"
        } else {
            "torrents/resume"
        };
        self.post(path, &[("hashes", id.as_str())]).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_table() {
        use DownloadState::*;
        for (s, want) in [
            ("error", Warning),
            ("missingFiles", Warning),
            ("pausedDL", Paused),
            ("stoppedDL", Paused),
            ("queuedDL", Queued),
            ("checkingDL", Queued),
            ("metaDL", Queued),
            ("stalledDL", Warning),
            ("downloading", Downloading),
            ("forcedDL", Downloading),
            ("uploading", Completed),
            ("stalledUP", Completed),
            ("pausedUP", Completed),
            ("stoppedUP", Completed),
            ("moving", PostProcessing),
            ("whatever", Warning),
        ] {
            assert_eq!(map_state(s).0, want, "{s}");
        }
    }

    fn t(state: &str, ratio: f64, ratio_limit: f64) -> QbTorrent {
        QbTorrent {
            hash: "ABC".into(),
            name: "n".into(),
            category: "tv".into(),
            state: state.into(),
            size: 10,
            amount_left: 0,
            eta: 8_640_000,
            progress: 1.0,
            save_path: "/dl/".into(),
            content_path: None,
            ratio,
            ratio_limit,
            seeding_time_limit: -2,
            seeding_time: Some(60),
            completion_on: 0,
        }
    }

    #[test]
    fn removability_follows_seed_limits() {
        let prefs = QbPreferences {
            max_ratio_enabled: true,
            max_ratio: 2.0,
            ..Default::default()
        };
        assert!(to_item(t("pausedUP", 2.0, -2.0), &prefs).can_be_removed);
        assert!(!to_item(t("pausedUP", 1.0, -2.0), &prefs).can_be_removed);
        assert!(!to_item(t("uploading", 5.0, -2.0), &prefs).can_be_removed);
        assert!(to_item(t("stoppedUP", 1.0, 1.0), &QbPreferences::default()).can_be_removed);
        assert!(!to_item(t("pausedUP", 9.0, -1.0), &prefs).can_be_removed);
        let item = to_item(t("pausedUP", 0.0, -2.0), &prefs);
        assert_eq!(item.output_path.as_deref(), Some("/dl/n"));
        assert_eq!(item.download_id.0, "abc");
        assert_eq!(item.eta_secs, None);
    }
}

/// The session cookie from a login answer's `name=value` pairs. qBittorrent before 5.1 calls it `SID`; 5.1 and
/// later call it `QBT_SID_<port>` by default (and let the name be changed), so either is taken, and otherwise the
/// only cookie set.
fn session_cookie(cookies: &[String]) -> Option<String> {
    let name = |kv: &String| kv.split('=').next().unwrap_or("").to_owned();
    cookies
        .iter()
        .find(|kv| name(kv) == "SID" || name(kv).starts_with("QBT_SID"))
        .or(if cookies.len() == 1 { cookies.first() } else { None })
        .cloned()
}

#[cfg(test)]
mod session_cookie_tests {
    use super::session_cookie;

    #[test]
    fn takes_the_old_and_the_new_names() {
        let c = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(session_cookie(&c(&["SID=abc"])).as_deref(), Some("SID=abc"));
        assert_eq!(
            session_cookie(&c(&["other=1", "QBT_SID_8080=abc"])).as_deref(),
            Some("QBT_SID_8080=abc")
        );
        assert_eq!(
            session_cookie(&c(&["mysession=abc"])).as_deref(),
            Some("mysession=abc")
        );
        assert_eq!(session_cookie(&c(&["a=1", "b=2"])), None);
        assert_eq!(session_cookie(&[]), None);
    }
}
