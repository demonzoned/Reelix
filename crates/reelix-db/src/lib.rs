//! Reelix persistence: SQLite pool, embedded migrations and typed repositories.
//!
//! Timestamps are stored as INTEGER unix milliseconds and exposed as `DateTime<Utc>`.

mod api_keys;
mod custom_formats;
mod delay_profiles;
mod error;
mod jobs;
mod quality_definitions;
mod quality_profiles;
mod release_profiles;
pub mod secrets;
mod sessions;
mod settings;
mod tasks;
mod users;

use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use sqlx::SqlitePool;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqliteLockingMode, SqlitePoolOptions, SqliteSynchronous,
};

pub use api_keys::{ApiKey, ApiKeyRepo};
pub use custom_formats::CustomFormatRepo;
pub use delay_profiles::DelayProfileRepo;
pub use error::DbError;
pub use jobs::{Job, JobRepo, NewJob};
pub use quality_definitions::QualityDefinitionRepo;
pub use quality_profiles::QualityProfileRepo;
pub use release_profiles::ReleaseProfileRepo;
pub use sessions::SessionRepo;
pub use settings::SettingsRepo;
pub use tasks::{ScheduledTask, TaskRepo};
pub use users::{NewUser, UserRepo};

pub type Result<T> = std::result::Result<T, DbError>;

/// SQLite's extended I/O errors for the WAL shared-memory file: SQLITE_IOERR_SHMOPEN (4618),
/// SQLITE_IOERR_SHMSIZE (4874) and SQLITE_IOERR_SHMMAP (5386).
fn no_shared_memory(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if matches!(d.code().as_deref(), Some("4618" | "4874" | "5386")))
}

/// The file beside the database that records that the write-ahead log cannot be used there.
fn no_wal_marker(db_file: &std::path::Path) -> std::path::PathBuf {
    let mut name = db_file.as_os_str().to_owned();
    name.push(".no-wal");
    std::path::PathBuf::from(name)
}

const NO_WAL_MARKER_TEXT: &str = "Reelix found that SQLite's write-ahead log cannot be used in this folder \
(its shared-memory file could not be used), so the database uses a rollback journal. Remove this file to \
try the write-ahead log again, for example after moving the data folder to a local disk.\n";

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations/sqlite");

/// Current time as unix milliseconds.
pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Converts unix milliseconds to a UTC timestamp (out-of-range values clamp to the epoch).
pub fn from_ms(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap_or_default()
}

/// Handle to the database; cheap to clone.
#[derive(Clone, Debug)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    /// Opens/creates the DB (create_if_missing, WAL, foreign_keys=on, busy_timeout 5s,
    /// synchronous=NORMAL, journal_size_limit 64 MiB) and runs migrations.
    ///
    /// WAL needs a memory-mapped `-shm` file. Some file systems cannot map one, such as a FUSE mount
    /// without page caching (mergerfs with `cache.files=off`, its default) or some network shares; SQLite
    /// then fails every read with a disk I/O error (SQLITE_IOERR_SHMMAP and its kin). On those the
    /// database falls back to a rollback journal, with a warning, instead of not starting.
    pub async fn connect(url: &str) -> Result<Db> {
        let opts = SqliteConnectOptions::from_str(url)?
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5))
            .synchronous(SqliteSynchronous::Normal);
        let file = opts.get_filename();
        if let Some(parent) = file.parent()
            && !parent.as_os_str().is_empty()
            && file.as_os_str() != ":memory:"
        {
            std::fs::create_dir_all(parent)?;
        }
        // An earlier start found that this file system cannot share memory: go straight to the
        // rollback journal, rather than set WAL and turn it back (rewriting the file) every start.
        let marker = no_wal_marker(file);
        if marker.exists() {
            tracing::warn!(
                file = %file.display(),
                marker = %marker.display(),
                "the write-ahead log is off for this database, as an earlier start found it could not \
                 be used here; remove the marker file to try it again, for example after moving the \
                 data folder to a local disk"
            );
            return Self::migrated(Self::open_without_shared_memory(opts).await?).await;
        }
        match Self::open(opts.clone().journal_mode(SqliteJournalMode::Wal)).await {
            Ok(pool) => Self::migrated(pool).await,
            Err(DbError::Sqlx(e)) if no_shared_memory(&e) => {
                tracing::warn!(
                    file = %file.display(),
                    error = %e,
                    "SQLite could not use the shared-memory file (-shm) beside the database, so the \
                     write-ahead log is off and a rollback journal is used. Either this file system \
                     cannot map shared memory (such as a FUSE pool or a network share), or the -shm \
                     file cannot be opened (check its owner and permissions). Keep the data folder on \
                     a local disk for the best speed and safety: on a network share, file locking can \
                     be unreliable whichever journal is used"
                );
                let pool = Self::open_without_shared_memory(opts).await?;
                if let Err(e) = std::fs::write(&marker, NO_WAL_MARKER_TEXT) {
                    tracing::warn!(marker = %marker.display(), error = %e, "could not remember that the write-ahead log is off");
                }
                Self::migrated(pool).await
            }
            Err(e) => Err(e),
        }
    }

    /// The pool, checked with one read: the shared memory of WAL is only mapped on the first read.
    async fn open(opts: SqliteConnectOptions) -> Result<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            // Shrink the write-ahead log back to at most 64 MiB after each checkpoint; without a limit it
            // keeps its largest size (after a big import or migration) on disk for good.
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("PRAGMA journal_size_limit = 67108864")
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(opts)
            .await?;
        if let Err(e) = sqlx::query("SELECT count(*) FROM sqlite_master")
            .execute(&pool)
            .await
        {
            pool.close().await;
            return Err(e.into());
        }
        Ok(pool)
    }

    /// A rollback journal in place of WAL. A database already in WAL mode can only be opened without
    /// shared memory under an exclusive lock, so one connection first turns it back (which also
    /// checkpoints the log into the file); the pool then opens it in DELETE mode.
    ///
    /// Without the write-ahead log, `synchronous=NORMAL` can lose or corrupt the latest writes on a
    /// power cut, so the rollback journal uses `FULL`.
    async fn open_without_shared_memory(opts: SqliteConnectOptions) -> Result<SqlitePool> {
        use sqlx::Connection;
        let opts = opts.synchronous(SqliteSynchronous::Full);
        let convert = opts
            .clone()
            .locking_mode(SqliteLockingMode::Exclusive)
            .journal_mode(SqliteJournalMode::Delete);
        sqlx::SqliteConnection::connect_with(&convert)
            .await?
            .close()
            .await?;
        Self::open(opts.journal_mode(SqliteJournalMode::Delete)).await
    }

    /// Private in-memory DB with migrations applied (single connection kept alive). For tests.
    pub async fn in_memory() -> Result<Db> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(opts)
            .await?;
        Self::migrated(pool).await
    }

    async fn migrated(pool: SqlitePool) -> Result<Db> {
        MIGRATOR.run(&pool).await?;
        Ok(Db { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Runs `PRAGMA optimize` (housekeeping).
    pub async fn optimize(&self) -> Result<()> {
        sqlx::query("PRAGMA optimize").execute(&self.pool).await?;
        Ok(())
    }

    pub fn users(&self) -> UserRepo<'_> {
        UserRepo { db: self }
    }

    pub fn sessions(&self) -> SessionRepo<'_> {
        SessionRepo { db: self }
    }

    pub fn api_keys(&self) -> ApiKeyRepo<'_> {
        ApiKeyRepo { db: self }
    }

    pub fn settings(&self) -> SettingsRepo<'_> {
        SettingsRepo { db: self }
    }

    pub fn jobs(&self) -> JobRepo<'_> {
        JobRepo { db: self }
    }

    pub fn tasks(&self) -> TaskRepo<'_> {
        TaskRepo { db: self }
    }

    pub fn quality_definitions(&self) -> QualityDefinitionRepo<'_> {
        QualityDefinitionRepo { db: self }
    }

    pub fn quality_profiles(&self) -> QualityProfileRepo<'_> {
        QualityProfileRepo { db: self }
    }

    pub fn custom_formats(&self) -> CustomFormatRepo<'_> {
        CustomFormatRepo { db: self }
    }

    pub fn delay_profiles(&self) -> DelayProfileRepo<'_> {
        DelayProfileRepo { db: self }
    }

    pub fn release_profiles(&self) -> ReleaseProfileRepo<'_> {
        ReleaseProfileRepo { db: self }
    }
}

fn decode_err(msg: impl Into<String>) -> DbError {
    DbError::Sqlx(sqlx::Error::Decode(msg.into().into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_roundtrip() {
        let now = now_ms();
        assert_eq!(from_ms(now).timestamp_millis(), now);
    }

    #[tokio::test]
    async fn in_memory_persists_across_queries() {
        let db = Db::in_memory().await.unwrap();
        db.ping().await.unwrap();
        db.settings().set("k", &1).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(db.settings().get::<i32>("k").await.unwrap(), Some(1));
    }

    #[tokio::test]
    async fn a_wal_database_reopens_without_shared_memory() {
        // What a file system that cannot map the -shm file gets: the WAL database from an earlier
        // start is turned back into a rollback journal, and its data is kept.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reelix.db");
        let url = format!("sqlite://{}", path.display());
        {
            let db = Db::connect(&url).await.unwrap();
            db.settings().set("kept", "yes").await.unwrap();
            db.pool().close().await;
        }
        let opts = SqliteConnectOptions::from_str(&url).unwrap().foreign_keys(true);
        let db = Db::migrated(Db::open_without_shared_memory(opts).await.unwrap())
            .await
            .unwrap();
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(mode, "delete");
        assert_eq!(
            db.settings().get::<String>("kept").await.unwrap().as_deref(),
            Some("yes")
        );
        assert!(!dir.path().join("reelix.db-wal").exists());
    }

    #[tokio::test]
    async fn a_remembered_fallback_opens_with_a_full_sync_rollback_journal() {
        // An earlier start fell back and left the marker: this start does not try WAL again, and
        // the rollback journal syncs fully.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reelix.db");
        let url = format!("sqlite://{}", path.display());
        {
            let db = Db::connect(&url).await.unwrap();
            db.settings().set("kept", "yes").await.unwrap();
            db.pool().close().await;
        }
        std::fs::write(no_wal_marker(&path), NO_WAL_MARKER_TEXT).unwrap();
        let db = Db::connect(&url).await.unwrap();
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(mode, "delete");
        let sync: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(sync, 2, "FULL");
        assert_eq!(
            db.settings().get::<String>("kept").await.unwrap().as_deref(),
            Some("yes")
        );
        assert_eq!(no_wal_marker(&path), dir.path().join("reelix.db.no-wal"));
    }

    #[test]
    fn only_the_shared_memory_errors_fall_back() {
        let other = sqlx::Error::RowNotFound;
        assert!(!no_shared_memory(&other));
    }

    #[tokio::test]
    async fn connect_file_db_creates_dirs_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("reelix.db");
        let url = format!("sqlite://{}", path.display());
        {
            let db = Db::connect(&url).await.unwrap();
            db.settings().set("x", "y").await.unwrap();
            db.optimize().await.unwrap();
            db.pool().close().await;
        }
        let db = Db::connect(&url).await.unwrap();
        assert_eq!(
            db.settings().get::<String>("x").await.unwrap().as_deref(),
            Some("y")
        );
    }
}
