//! Bootstrap configuration: defaults < `config.toml` < `REELIX_*` env vars < CLI flags.

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
pub use reelix_api::SecureCookies;
use serde::{Deserialize, Serialize};

/// Default HTTP port.
pub const DEFAULT_PORT: u16 = 7700;
/// Default data directory when neither `--data-dir` nor `REELIX_DATA_DIR` is given.
pub const DEFAULT_DATA_DIR: &str = "./data";
/// Name of the config file inside the data directory.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Configuration error (boxed: `figment::Error` is large).
pub type ConfigError = Box<figment::Error>;

/// Overrides coming from command-line flags (highest precedence).
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub config: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub port: Option<u16>,
    pub bind: Option<IpAddr>,
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Pretty,
    Json,
}

/// Shape of `config.toml` (and of the merged figment). Derived paths stay `None` until resolved.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct RawConfig {
    server: RawServer,
    database: RawDatabase,
    paths: RawPaths,
    security: RawSecurity,
    logging: RawLogging,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct RawServer {
    bind: IpAddr,
    port: u16,
    url_base: String,
}

impl Default for RawServer {
    fn default() -> Self {
        Self {
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: DEFAULT_PORT,
            url_base: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct RawDatabase {
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct RawPaths {
    data_dir: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    log_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    web_dir: Option<PathBuf>,
}

impl Default for RawPaths {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from(DEFAULT_DATA_DIR),
            backup_dir: None,
            log_dir: None,
            web_dir: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct RawSecurity {
    #[serde(skip_serializing_if = "Option::is_none")]
    secret_key_file: Option<PathBuf>,
    trusted_proxies: Vec<String>,
    secure_cookies: SecureCookies,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct RawLogging {
    level: String,
    format: LogFormat,
}

impl Default for RawLogging {
    fn default() -> Self {
        Self {
            level: "info".into(),
            format: LogFormat::Pretty,
        }
    }
}

/// Fully resolved configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub bind: IpAddr,
    pub port: u16,
    /// `""` or `"/segment[/segment…]"`, never with a trailing slash.
    pub url_base: String,
    pub database_url: String,
    pub data_dir: PathBuf,
    pub backup_dir: PathBuf,
    pub log_dir: PathBuf,
    /// Explicitly configured web UI directory; `None` means auto-detect.
    pub web_dir: Option<PathBuf>,
    pub secret_key_file: PathBuf,
    /// Reverse proxies (CIDRs) whose `X-Forwarded-For` is believed for the client address.
    pub trusted_proxies: Vec<reelix_api::IpNet>,
    pub secure_cookies: SecureCookies,
    pub log_level: String,
    pub log_format: LogFormat,
    /// Config file that was read, if any.
    pub config_file: Option<PathBuf>,
    /// Settings forced by command-line flags (`port`, `bind`, `data_dir`).
    pub cli_overrides: Vec<&'static str>,
}

impl Config {
    /// Loads the configuration from all sources, reading `REELIX_*` from the process environment.
    pub fn load(cli: &CliOverrides) -> Result<Config, ConfigError> {
        // The data dir decides where config.toml lives, so resolve it first (CLI > env > default).
        let data_dir = cli
            .data_dir
            .clone()
            .or_else(|| {
                std::env::var_os("REELIX_DATA_DIR")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR));

        let (config_file, explicit) = match &cli.config {
            Some(path) => (path.clone(), true),
            None => (data_dir.join(CONFIG_FILE_NAME), false),
        };

        let mut figment = Figment::from(Serialized::defaults(RawConfig {
            paths: RawPaths {
                data_dir,
                ..RawPaths::default()
            },
            ..RawConfig::default()
        }));

        let config_file = if config_file.is_file() {
            figment = figment.merge(Toml::file_exact(&config_file));
            Some(config_file)
        } else if explicit {
            return Err(figment::Error::from(format!(
                "config file {} does not exist",
                config_file.display()
            ))
            .into());
        } else {
            None
        };

        figment =
            figment.merge(Env::prefixed("REELIX_").filter_map(|key| env_key(key.as_str()).map(Into::into)));

        if let Some(dir) = &cli.data_dir {
            figment = figment.merge(Serialized::default("paths.data_dir", dir));
        }
        if let Some(port) = cli.port {
            figment = figment.merge(Serialized::default("server.port", port));
        }
        if let Some(bind) = cli.bind {
            figment = figment.merge(Serialized::default("server.bind", bind));
        }

        let raw: RawConfig = figment.extract()?;
        let mut config = Self::resolve(raw, config_file)?;
        config.cli_overrides = [
            ("port", cli.port.is_some()),
            ("bind", cli.bind.is_some()),
            ("data_dir", cli.data_dir.is_some()),
        ]
        .into_iter()
        .filter_map(|(key, set)| set.then_some(key))
        .collect();
        Ok(config)
    }

    fn resolve(raw: RawConfig, config_file: Option<PathBuf>) -> Result<Config, ConfigError> {
        let url_base =
            normalize_url_base(&raw.server.url_base).map_err(|e| Box::new(figment::Error::from(e)))?;
        let trusted_proxies = raw
            .security
            .trusted_proxies
            .iter()
            .map(|p| p.parse())
            .collect::<Result<Vec<_>, String>>()
            .map_err(|e| Box::new(figment::Error::from(format!("security.trusted_proxies: {e}"))))?;
        let data_dir = raw.paths.data_dir;
        let database_url = raw
            .database
            .url
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| default_database_url(&data_dir));
        Ok(Config {
            bind: raw.server.bind,
            port: raw.server.port,
            url_base,
            database_url,
            backup_dir: raw.paths.backup_dir.unwrap_or_else(|| data_dir.join("backups")),
            log_dir: raw.paths.log_dir.unwrap_or_else(|| data_dir.join("logs")),
            web_dir: raw.paths.web_dir.filter(|p| !p.as_os_str().is_empty()),
            secret_key_file: raw
                .security
                .secret_key_file
                .unwrap_or_else(|| data_dir.join("secret.key")),
            trusted_proxies,
            secure_cookies: raw.security.secure_cookies,
            log_level: raw.logging.level,
            log_format: raw.logging.format,
            data_dir,
            config_file,
            cli_overrides: Vec::new(),
        })
    }

    /// Database backend name derived from the URL scheme (`sqlite`, `postgres`, …).
    pub fn database_kind(&self) -> &str {
        self.database_url
            .split_once(':')
            .map_or("sqlite", |(scheme, _)| scheme)
    }

    /// Path of the config file in the data directory (where the default one is written).
    pub fn default_config_path(&self) -> PathBuf {
        self.data_dir.join(CONFIG_FILE_NAME)
    }
}

/// Maps a `REELIX_`-stripped, lowercased env var name to its config key path.
pub(crate) fn env_key(key: &str) -> Option<&'static str> {
    Some(match key.to_ascii_lowercase().as_str() {
        "port" => "server.port",
        "bind" => "server.bind",
        "url_base" => "server.url_base",
        "database_url" => "database.url",
        "data_dir" => "paths.data_dir",
        "backup_dir" => "paths.backup_dir",
        "log_dir" => "paths.log_dir",
        "web_dir" => "paths.web_dir",
        "secret_key_file" => "security.secret_key_file",
        "secure_cookies" => "security.secure_cookies",
        "log_level" => "logging.level",
        "log_format" => "logging.format",
        _ => return None,
    })
}

fn default_database_url(data_dir: &Path) -> String {
    let path = data_dir.to_string_lossy().replace('\\', "/");
    format!("sqlite://{}/reelix.db", path.trim_end_matches('/'))
}

/// Normalises a URL base to `""` or `"/a[/b…]"` (leading slash, no trailing slash).
pub fn normalize_url_base(input: &str) -> Result<String, String> {
    let segments: Vec<&str> = input.trim().split('/').filter(|s| !s.is_empty()).collect();
    for seg in &segments {
        let ok = seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'));
        if !ok || *seg == "." || *seg == ".." {
            return Err(format!(
                "invalid url_base {input:?}: only [A-Za-z0-9._~-] path segments are allowed"
            ));
        }
    }
    Ok(segments.iter().map(|s| format!("/{s}")).collect())
}

/// Commented template written to `{data_dir}/config.toml` on first run.
pub fn default_config_template() -> String {
    format!(
        r#"# Reelix bootstrap configuration.
# Precedence: CLI flags > REELIX_* environment variables > this file > defaults.
# Most settings live in the database and are edited in the web UI.

[server]
# bind = "0.0.0.0"
# port = {DEFAULT_PORT}
# Serve under a sub-path behind a reverse proxy, e.g. "/reelix".
# url_base = ""

[database]
# Defaults to sqlite://<data_dir>/reelix.db
# url = "sqlite:///config/reelix.db"

[paths]
# backup_dir = "<data_dir>/backups"
# log_dir = "<data_dir>/logs"
# Directory with the built web UI (defaults to ./web, then crates/reelix-web/dist).
# web_dir = "/app/web"

[security]
# Key used to encrypt provider secrets at rest; generated on first run if absent.
# secret_key_file = "<data_dir>/secret.key"
# Reverse proxies whose X-Forwarded-For header is trusted for the client address
# (login rate limiting, local-only checks).
# trusted_proxies = ["172.16.0.0/12"]
# Secure flag on session cookies: auto (when X-Forwarded-Proto is https) | always | never
# secure_cookies = "auto"

[logging]
# trace | debug | info | warn | error (RUST_LOG overrides)
# level = "info"
# pretty | json
# format = "pretty"
"#
    )
}

/// Writes the default config file if `path` doesn't exist yet. Returns true if written.
pub fn write_default_config_if_missing(path: &Path) -> std::io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, default_config_template())?;
    Ok(true)
}

/// Whether a metadata base URL is plain `http://` to another computer. The TMDB v3 key travels in
/// the address (`?api_key=`), so it would cross the network in clear text and land in proxy logs.
pub fn plain_http_to_another_computer(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    if u.scheme() != "http" {
        return false;
    }
    let Some(host) = u.host_str() else {
        return false;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let ip = host.trim_start_matches('[').trim_end_matches(']');
    !(host == "localhost"
        || host.ends_with(".localhost")
        || ip.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))
}

impl Config {
    /// Warns about each metadata base URL that sends keys in clear text to another computer.
    pub fn warn_about_plain_http_metadata(&self) {
        for (name, url) in [
            ("metadata.tmdb_base_url", &self.tmdb_base_url),
            ("metadata.tvdb_base_url", &self.tvdb_base_url),
        ] {
            if let Some(u) = url
                && plain_http_to_another_computer(u)
            {
                tracing::warn!(
                    setting = name,
                    "{name} uses plain http to another computer, so API keys cross the network \
                     unencrypted; use https, or a proxy on this computer"
                );
            }
        }
    }
}

#[cfg(test)]
// `figment::Jail` closures must return the (large) `figment::Result`.
#[allow(clippy::result_large_err)]
mod tests {
    use super::*;
    use figment::Jail;

    fn load(cli: &CliOverrides) -> figment::Result<Config> {
        Config::load(cli).map_err(|e| *e)
    }

    #[test]
    fn plain_http_metadata_to_another_computer_is_noticed() {
        assert!(plain_http_to_another_computer("http://mirror:8000/3"));
        assert!(plain_http_to_another_computer("http://192.168.1.5/3"));
        assert!(!plain_http_to_another_computer("https://mirror/3"));
        assert!(!plain_http_to_another_computer("http://localhost:8000/3"));
        assert!(!plain_http_to_another_computer("http://127.0.0.1:8000/3"));
        assert!(!plain_http_to_another_computer("http://[::1]:8000/3"));
    }

    #[test]
    fn url_base_normalisation() {
        assert_eq!(normalize_url_base("").unwrap(), "");
        assert_eq!(normalize_url_base("/").unwrap(), "");
        assert_eq!(normalize_url_base("  ").unwrap(), "");
        assert_eq!(normalize_url_base("reelix").unwrap(), "/reelix");
        assert_eq!(normalize_url_base("/reelix/").unwrap(), "/reelix");
        assert_eq!(normalize_url_base("//a//b/").unwrap(), "/a/b");
        assert!(normalize_url_base("/a\"><script>").is_err());
        assert!(normalize_url_base("/../x").is_err());
    }

    #[test]
    fn defaults_without_any_source() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let cfg = load(&CliOverrides::default())?;
            assert_eq!(cfg.port, 7700);
            assert_eq!(cfg.bind, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
            assert_eq!(cfg.url_base, "");
            assert_eq!(cfg.data_dir, PathBuf::from("./data"));
            assert_eq!(cfg.database_url, "sqlite://./data/reelix.db");
            assert_eq!(cfg.log_dir, PathBuf::from("./data/logs"));
            assert_eq!(cfg.backup_dir, PathBuf::from("./data/backups"));
            assert_eq!(cfg.secret_key_file, PathBuf::from("./data/secret.key"));
            assert_eq!(cfg.log_format, LogFormat::Pretty);
            assert_eq!(cfg.config_file, None);
            assert_eq!(cfg.database_kind(), "sqlite");
            Ok(())
        });
    }

    #[test]
    fn precedence_defaults_file_env_cli() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_dir("data")?;
            jail.create_file(
                "data/config.toml",
                r#"
                [server]
                port = 8001
                bind = "127.0.0.1"
                url_base = "reelix/"
                [logging]
                level = "debug"
                format = "json"
                [security]
                trusted_proxies = ["10.0.0.0/8"]
                secure_cookies = "always"
                "#,
            )?;

            // File overrides defaults.
            let cfg = load(&CliOverrides::default())?;
            assert_eq!(cfg.port, 8001);
            assert_eq!(cfg.bind, "127.0.0.1".parse::<IpAddr>().unwrap());
            assert_eq!(cfg.url_base, "/reelix");
            assert_eq!(cfg.log_level, "debug");
            assert_eq!(cfg.log_format, LogFormat::Json);
            assert_eq!(cfg.trusted_proxies, vec!["10.0.0.0/8".parse().unwrap()]);
            assert_eq!(cfg.secure_cookies, SecureCookies::Always);
            assert!(cfg.config_file.is_some());

            // Env overrides file.
            jail.set_env("REELIX_PORT", "8002");
            jail.set_env("REELIX_URL_BASE", "/other");
            jail.set_env("REELIX_LOG_LEVEL", "warn");
            jail.set_env("REELIX_DATABASE_URL", "sqlite://elsewhere.db");
            jail.set_env("REELIX_TMDB_API_KEY", "ignored");
            let cfg = load(&CliOverrides::default())?;
            assert_eq!(cfg.port, 8002);
            assert_eq!(cfg.url_base, "/other");
            assert_eq!(cfg.log_level, "warn");
            assert_eq!(cfg.database_url, "sqlite://elsewhere.db");
            assert_eq!(cfg.bind, "127.0.0.1".parse::<IpAddr>().unwrap());

            // CLI overrides env.
            let cli = CliOverrides {
                port: Some(8003),
                bind: Some("::1".parse().unwrap()),
                ..CliOverrides::default()
            };
            let cfg = load(&cli)?;
            assert_eq!(cfg.port, 8003);
            assert_eq!(cfg.bind, "::1".parse::<IpAddr>().unwrap());
            assert_eq!(cfg.url_base, "/other");
            Ok(())
        });
    }

    #[test]
    fn data_dir_selects_config_file_and_derived_paths() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_dir("cfg")?;
            jail.create_file("cfg/config.toml", "[server]\nport = 9100\n")?;

            jail.set_env("REELIX_DATA_DIR", "cfg");
            let cfg = load(&CliOverrides::default())?;
            assert_eq!(cfg.port, 9100);
            assert_eq!(cfg.data_dir, PathBuf::from("cfg"));
            assert_eq!(cfg.database_url, "sqlite://cfg/reelix.db");
            assert_eq!(cfg.log_dir, PathBuf::from("cfg/logs"));

            // --data-dir beats REELIX_DATA_DIR; no config file there.
            let cli = CliOverrides {
                data_dir: Some("other".into()),
                ..CliOverrides::default()
            };
            let cfg = load(&cli)?;
            assert_eq!(cfg.port, 7700);
            assert_eq!(cfg.data_dir, PathBuf::from("other"));
            assert_eq!(cfg.secret_key_file, PathBuf::from("other/secret.key"));
            Ok(())
        });
    }

    #[test]
    fn explicit_config_file_must_exist() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let cli = CliOverrides {
                config: Some("missing.toml".into()),
                ..CliOverrides::default()
            };
            assert!(load(&cli).is_err());

            jail.create_file(
                "custom.toml",
                "[paths]\ndata_dir = \"/srv/reelix\"\nweb_dir = \"/app/web\"\n",
            )?;
            let cli = CliOverrides {
                config: Some("custom.toml".into()),
                ..CliOverrides::default()
            };
            let cfg = load(&cli)?;
            assert_eq!(cfg.data_dir, PathBuf::from("/srv/reelix"));
            assert_eq!(cfg.database_url, "sqlite:///srv/reelix/reelix.db");
            assert_eq!(cfg.web_dir, Some(PathBuf::from("/app/web")));
            Ok(())
        });
    }

    #[test]
    fn invalid_values_are_rejected() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("REELIX_URL_BASE", "/bad\"path");
            assert!(load(&CliOverrides::default()).is_err());
            jail.set_env("REELIX_URL_BASE", "");
            jail.set_env("REELIX_LOG_FORMAT", "xml");
            assert!(load(&CliOverrides::default()).is_err());
            jail.set_env("REELIX_LOG_FORMAT", "json");
            jail.set_env("REELIX_SECURE_COOKIES", "sometimes");
            assert!(load(&CliOverrides::default()).is_err());
            Ok(())
        });
    }

    #[test]
    fn default_template_parses_and_is_written_once() {
        let parsed: RawConfig = toml_parse(&default_config_template());
        assert_eq!(parsed.server.port, DEFAULT_PORT);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        assert!(write_default_config_if_missing(&path).unwrap());
        std::fs::write(&path, "# mine").unwrap();
        assert!(!write_default_config_if_missing(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# mine");
    }

    fn toml_parse(s: &str) -> RawConfig {
        Figment::from(Serialized::defaults(RawConfig::default()))
            .merge(Toml::string(s))
            .extract()
            .unwrap()
    }
}
