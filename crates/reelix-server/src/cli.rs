//! Command-line interface.

use std::net::IpAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use reelix_api_types::migrate::SourceKind;

use crate::config::CliOverrides;

/// Reelix: movies, series, indexers and requests in one server.
#[derive(Debug, Parser)]
#[command(name = "reelix", version = reelix_core::VERSION, about)]
pub struct Cli {
    /// Path to config.toml (default: <data-dir>/config.toml).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Data directory for the database, keys and logs (default: ./data).
    #[arg(long, global = true, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    /// HTTP port (default: 7700).
    #[arg(long, global = true)]
    pub port: Option<u16>,
    /// Address to bind (default: 0.0.0.0).
    #[arg(long, global = true, value_name = "IP")]
    pub bind: Option<IpAddr>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Probe the local health endpoint; exits 0 iff it returns HTTP 200.
    Healthcheck,
    /// Authenticate Spotify with native OAuth/PKCE and save reusable session credentials.
    SpotifyLogin {
        #[arg(long)]
        credentials: PathBuf,
        /// Optional Spotify developer application client ID.
        #[arg(long)]
        client_id: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:8898/login")]
        redirect_uri: String,
    },
    /// Ask the install wizard's questions in this terminal, for a server with no desktop. Needs
    /// Reelix running (with Docker: `docker exec -it reelix /app/reelix setup`).
    Setup {
        /// Plain lines that scroll, with nothing redrawn: for screen readers (or REELIX_PLAIN=1).
        #[arg(long)]
        plain: bool,
        /// Reelix's address (default: this server, on --port).
        #[arg(long, value_name = "URL")]
        url: Option<String>,
    },
    /// Manage the Reelix Windows service (Windows only).
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Back up the database, config.toml and secret key of `--data-dir`.
    Backup {
        #[command(subcommand)]
        action: BackupAction,
    },
    /// Check or stage a backup archive (`reelix_backup_*.zip`) for restoring.
    Restore {
        #[command(subcommand)]
        action: RestoreAction,
    },
    /// Save apps to migrate from for the admin to review in System → Migrate.
    Migrate {
        #[command(subcommand)]
        action: MigrateAction,
    },
    /// Edit config.toml of `--data-dir`.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum BackupAction {
    /// Write a new backup to the backup folder (`<data-dir>/backups`) and print its path. Works
    /// while Reelix runs, too. Creates the database if there is none yet.
    Create,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum RestoreAction {
    /// Check that the archive is a Reelix backup, without changing anything.
    Check { archive: PathBuf },
    /// Validate the archive and copy it to `<data-dir>/restore` as the pending restore, like
    /// uploading it in System → Backup: Reelix restores it on its next start (and rolls back if
    /// the restored database won't open). Exits non-zero, with the reason on stderr, if the
    /// archive isn't a valid backup.
    Stage { archive: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum MigrateAction {
    /// Save a source (URL and API key, encrypted with the secret key) to migrate from. Nothing is
    /// imported: after setup, System → Migrate offers it pre-filled for preview and run. Creates
    /// the database if there is none yet.
    Prepare {
        /// radarr, sonarr, prowlarr, lidarr, readarr or jellyseerr.
        #[arg(long, value_parser = parse_source)]
        source: SourceKind,
        /// e.g. http://localhost:7878 (include any URL base).
        #[arg(long)]
        url: String,
        /// File holding the API key (not passed on the command line, where other users of the
        /// computer could see it in the process list).
        #[arg(long, value_name = "PATH")]
        api_key_file: PathBuf,
    },
    /// Print the saved sources (keys masked) as JSON.
    List,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum ConfigAction {
    /// After copying a data folder from FROM to `--data-dir`: point the locations in config.toml
    /// that are inside FROM (data_dir, backup_dir, log_dir, secret_key_file, the SQLite database
    /// URL) at `--data-dir`. Everything else, comments included, is kept.
    Relocate {
        /// The old data folder.
        #[arg(long, value_name = "DIR")]
        from: PathBuf,
    },
}

fn parse_source(s: &str) -> Result<SourceKind, String> {
    SourceKind::parse(&s.to_ascii_lowercase()).ok_or_else(|| {
        let all: Vec<_> = SourceKind::ALL.iter().map(|k| k.as_str()).collect();
        format!("expected one of {}", all.join(", "))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum ServiceAction {
    /// Register and start the "Reelix" service (`--data-dir`, default C:\ProgramData\Reelix, and
    /// `--port` are passed on to it). Needs an elevated prompt. Idempotent: an existing service is
    /// reconfigured (and started if stopped). Exits non-zero, with the error on stderr, on failure.
    Install,
    /// Stop and remove the service; the data directory is kept.
    Uninstall,
    /// Run as the service (started by the Windows service manager).
    Run,
}

impl Cli {
    pub fn overrides(&self) -> CliOverrides {
        CliOverrides {
            config: self.config.clone(),
            data_dir: self.data_dir.clone(),
            port: self.port,
            bind: self.bind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_valid_and_parses() {
        Cli::command().debug_assert();
        let cli = Cli::parse_from([
            "reelix",
            "--port",
            "8000",
            "--bind",
            "127.0.0.1",
            "--data-dir",
            "/x",
        ]);
        assert_eq!(cli.port, Some(8000));
        assert!(cli.command.is_none());
        let cli = Cli::parse_from(["reelix", "healthcheck", "--port", "9"]);
        assert!(matches!(cli.command, Some(Command::Healthcheck)));
        assert_eq!(cli.overrides().port, Some(9));
        let cli = Cli::parse_from(["reelix", "spotify-login", "--credentials", "credentials.json"]);
        assert!(matches!(cli.command, Some(Command::SpotifyLogin { .. })));
        assert!(Cli::try_parse_from(["reelix", "spotify-login"]).is_err());
        let cli = Cli::parse_from([
            "reelix",
            "service",
            "install",
            "--data-dir",
            r"C:\ProgramData\Reelix",
            "--port",
            "7701",
        ]);
        assert!(matches!(
            cli.command,
            Some(Command::Service {
                action: ServiceAction::Install
            })
        ));
        assert_eq!(cli.port, Some(7701));
        assert!(cli.data_dir.is_some());
        let cli = Cli::parse_from(["reelix", "service", "run", "--data-dir", "x"]);
        assert!(matches!(
            cli.command,
            Some(Command::Service {
                action: ServiceAction::Run
            })
        ));
        assert!(Cli::try_parse_from(["reelix", "service"]).is_err());

        let cli = Cli::parse_from(["reelix", "restore", "stage", "b.zip", "--data-dir", "d"]);
        assert!(matches!(
            &cli.command,
            Some(Command::Restore { action: RestoreAction::Stage { archive } }) if archive.as_os_str() == "b.zip"
        ));
        assert!(cli.data_dir.is_some());
        assert!(Cli::try_parse_from(["reelix", "restore", "stage"]).is_err());
        let cli = Cli::parse_from(["reelix", "backup", "create", "--data-dir", "d"]);
        assert!(matches!(
            cli.command,
            Some(Command::Backup {
                action: BackupAction::Create
            })
        ));
        let cli = Cli::parse_from([
            "reelix",
            "migrate",
            "prepare",
            "--source",
            "Radarr",
            "--url",
            "http://localhost:7878",
            "--api-key-file",
            "k.txt",
        ]);
        assert!(matches!(
            cli.command,
            Some(Command::Migrate {
                action: MigrateAction::Prepare {
                    source: SourceKind::Radarr,
                    ..
                }
            })
        ));
        // The key itself is never a command-line argument.
        assert!(
            Cli::try_parse_from([
                "reelix",
                "migrate",
                "prepare",
                "--source",
                "radarr",
                "--url",
                "http://x",
                "--api-key",
                "secret"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "reelix",
                "migrate",
                "prepare",
                "--source",
                "plex",
                "--url",
                "http://x",
                "--api-key-file",
                "k"
            ])
            .is_err()
        );
        let cli = Cli::parse_from([
            "reelix", "--config", "c.toml", "config", "relocate", "--from", "old",
        ]);
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                action: ConfigAction::Relocate { .. }
            })
        ));
        assert!(cli.config.is_some());
        assert!(Cli::try_parse_from(["reelix", "--port", "notaport"]).is_err());
    }
}
