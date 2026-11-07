//! Backup archives (database snapshot, config file, secret key) and staged restores.
//!
//! A backup is a zip with `manifest.json`, `reelix.db` (a `VACUUM INTO` snapshot, consistent
//! while the server runs) and, when present, `config.toml` and `secret.key`. A restore is only
//! staged while running ([`stage`]); [`apply_pending_restore`] swaps the files in on the next
//! start, before the database is opened, and rolls back if the restored database won't open.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use chrono::{DateTime, NaiveDateTime, Utc};
use reelix_api_types::system::{BackupDto, BackupKind, PendingRestoreDto};
use reelix_db::secrets::{create_private_file, restrict_to_owner};
use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table};
use zip::write::SimpleFileOptions;

use super::Paths;

const MANIFEST: &str = "manifest.json";
const DB_ENTRY: &str = "reelix.db";
const CONFIG_ENTRY: &str = "config.toml";
const SECRET_ENTRY: &str = "secret.key";
const NAME_PREFIX: &str = "reelix_backup_";
const NAME_TIME: &str = "%Y.%m.%d_%H.%M.%S";
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
/// Folder inside the data directory holding a staged restore and pre-restore copies.
const RESTORE_DIR: &str = "restore";
const PENDING_ZIP: &str = "pending.zip";
const PENDING_INFO: &str = "pending.json";

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    app: String,
    version: String,
    created_at: DateTime<Utc>,
    kind: BackupKind,
}

/// File names are restricted so they are safe as path components and in HTTP headers.
pub fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn kind_str(kind: BackupKind) -> &'static str {
    match kind {
        BackupKind::Manual => "manual",
        BackupKind::Scheduled => "scheduled",
    }
}

fn parse_name(name: &str) -> Option<(BackupKind, DateTime<Utc>)> {
    let rest = name.strip_prefix(NAME_PREFIX)?.strip_suffix(".zip")?;
    let (kind, time) = rest.split_once('_')?;
    let kind = match kind {
        "manual" => BackupKind::Manual,
        "scheduled" => BackupKind::Scheduled,
        _ => return None,
    };
    // A `-N` suffix disambiguates backups made within the same second.
    let time = time.split_once('-').map_or(time, |(t, _)| t);
    let time = NaiveDateTime::parse_from_str(time, NAME_TIME).ok()?.and_utc();
    Some((kind, time))
}

fn read_manifest(path: &Path) -> Option<Manifest> {
    let mut archive = zip::ZipArchive::new(File::open(path).ok()?).ok()?;
    let entry = archive.by_name(MANIFEST).ok()?;
    serde_json::from_reader(entry.take(64 * 1024)).ok()
}

/// Every backup in the backup folder, newest first.
pub fn list(paths: &Paths) -> anyhow::Result<Vec<BackupDto>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&paths.backup_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("reading {}", paths.backup_dir.display())),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_safe_name(&name) || !name.ends_with(".zip") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let manifest = read_manifest(&entry.path());
        let parsed = parse_name(&name);
        let created_at = manifest
            .as_ref()
            .map(|m| m.created_at)
            .or(parsed.map(|p| p.1))
            .or_else(|| meta.modified().ok().map(DateTime::<Utc>::from))
            .unwrap_or_default();
        let kind = manifest
            .as_ref()
            .map(|m| m.kind)
            .or(parsed.map(|p| p.0))
            .unwrap_or(BackupKind::Manual);
        out.push(BackupDto {
            name,
            kind,
            size_bytes: meta.len(),
            created_at,
            version: manifest.map(|m| m.version),
        });
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.name.cmp(&a.name)));
    Ok(out)
}

/// Writes a new backup archive from a database snapshot already taken at `snapshot`.
pub fn write_archive(
    paths: &Paths,
    snapshot: &Path,
    kind: BackupKind,
    now: DateTime<Utc>,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(&paths.backup_dir)
        .with_context(|| format!("creating {}", paths.backup_dir.display()))?;
    restrict_to_owner(&paths.backup_dir);
    let stamp = now.format(NAME_TIME);
    let mut name = format!("{NAME_PREFIX}{}_{stamp}.zip", kind_str(kind));
    let mut n = 1;
    while paths.backup_dir.join(&name).exists() {
        n += 1;
        name = format!("{NAME_PREFIX}{}_{stamp}-{n}.zip", kind_str(kind));
    }
    let target = paths.backup_dir.join(&name);
    let partial = paths.backup_dir.join(format!(".{name}.partial"));
    let result = (|| -> anyhow::Result<()> {
        let mut zip = zip::ZipWriter::new(create_private_file(&partial)?);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let manifest = Manifest {
            app: "reelix".into(),
            version: reelix_core::VERSION.into(),
            created_at: now,
            kind,
        };
        zip.start_file(MANIFEST, opts)?;
        zip.write_all(&serde_json::to_vec_pretty(&manifest)?)?;
        zip.start_file(DB_ENTRY, opts)?;
        std::io::copy(&mut File::open(snapshot)?, &mut zip)?;
        if paths.config_file.is_file() {
            zip.start_file(CONFIG_ENTRY, opts)?;
            std::io::copy(&mut File::open(&paths.config_file)?, &mut zip)?;
        }
        if paths.backup_secret_key && paths.secret_key_file.is_file() {
            zip.start_file(SECRET_ENTRY, opts)?;
            std::io::copy(&mut File::open(&paths.secret_key_file)?, &mut zip)?;
        }
        zip.finish()?.sync_all()?;
        std::fs::rename(&partial, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result.with_context(|| format!("writing backup {}", target.display()))?;
    Ok(target)
}

/// Deletes the oldest scheduled backups beyond `keep`; returns how many were removed.
pub fn prune_scheduled(paths: &Paths, keep: usize) -> anyhow::Result<usize> {
    let scheduled: Vec<_> = list(paths)?
        .into_iter()
        .filter(|b| b.kind == BackupKind::Scheduled)
        .collect();
    let mut removed = 0;
    for old in scheduled.iter().skip(keep.max(1)) {
        match std::fs::remove_file(paths.backup_dir.join(&old.name)) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!(name = %old.name, error = %e, "could not delete old backup"),
        }
    }
    Ok(removed)
}

/// Checks that `path` is a Reelix backup with a SQLite database inside.
pub fn validate(path: &Path) -> anyhow::Result<()> {
    let file = File::open(path).context("opening the archive")?;
    let mut archive = zip::ZipArchive::new(file).context("this is not a zip file")?;
    if let Ok(entry) = archive.by_name(MANIFEST) {
        let manifest: Manifest =
            serde_json::from_reader(entry.take(64 * 1024)).context("the backup manifest is unreadable")?;
        if manifest.app != "reelix" {
            bail!("this is not a Reelix backup");
        }
    }
    let mut db = archive
        .by_name(DB_ENTRY)
        .map_err(|_| anyhow::anyhow!("the archive has no {DB_ENTRY}; it is not a Reelix backup"))?;
    // The declared size can lie; `extract` enforces the cap while writing too.
    if db.size() > max_entry_bytes(DB_ENTRY) {
        bail!("{DB_ENTRY} in the archive is too large");
    }
    let mut magic = [0u8; 16];
    db.read_exact(&mut magic)
        .ok()
        .filter(|()| &magic == SQLITE_MAGIC)
        .ok_or_else(|| anyhow::anyhow!("{DB_ENTRY} in the archive is not a SQLite database"))?;
    Ok(())
}

fn restore_dir(paths: &Paths) -> PathBuf {
    paths.data_dir.join(RESTORE_DIR)
}

#[derive(Debug, Serialize, Deserialize)]
struct PendingInfo {
    source: String,
    staged_at: DateTime<Utc>,
}

/// Validates `archive` and stages it (copied, so the original can be deleted) for the next start.
pub fn stage(
    paths: &Paths,
    archive: &Path,
    source: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<PendingRestoreDto> {
    validate(archive)?;
    let dir = restore_dir(paths);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let partial = dir.join(format!("{PENDING_ZIP}.partial"));
    std::fs::copy(archive, &partial).context("copying the backup")?;
    commit(paths, &partial, source, now)
}

/// A fresh path in the restore directory (created if needed) to save an upload to; it is on
/// the same filesystem as the staged archive, so [`stage_upload`] can move it into place.
pub fn upload_path(paths: &Paths) -> std::io::Result<PathBuf> {
    let dir = restore_dir(paths);
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("upload-{}.partial", rand::random::<u64>())))
}

/// Validates an uploaded archive saved at `upload` (see [`upload_path`]) and moves it into place.
pub fn stage_upload(paths: &Paths, upload: &Path, now: DateTime<Utc>) -> anyhow::Result<PendingRestoreDto> {
    validate(upload)?;
    commit(paths, upload, "upload", now)
}

/// Moves a validated archive in the restore directory into place as the pending restore.
fn commit(
    paths: &Paths,
    archive: &Path,
    source: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<PendingRestoreDto> {
    let dir = restore_dir(paths);
    std::fs::rename(archive, dir.join(PENDING_ZIP))?;
    let info = PendingInfo {
        source: source.to_owned(),
        staged_at: now,
    };
    std::fs::write(dir.join(PENDING_INFO), serde_json::to_vec(&info)?)?;
    Ok(PendingRestoreDto {
        source: info.source,
        staged_at: info.staged_at,
    })
}

pub fn pending(paths: &Paths) -> Option<PendingRestoreDto> {
    let dir = restore_dir(paths);
    if !dir.join(PENDING_ZIP).is_file() {
        return None;
    }
    let info: Option<PendingInfo> = std::fs::read(dir.join(PENDING_INFO))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    Some(match info {
        Some(i) => PendingRestoreDto {
            source: i.source,
            staged_at: i.staged_at,
        },
        None => PendingRestoreDto {
            source: "unknown".into(),
            staged_at: Utc::now(),
        },
    })
}

pub fn cancel(paths: &Paths) -> std::io::Result<()> {
    let dir = restore_dir(paths);
    for f in [PENDING_ZIP, PENDING_INFO] {
        match std::fs::remove_file(dir.join(f)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// SQLite sidecar files that belong with the database file.
fn db_files(db: &Path) -> Vec<PathBuf> {
    let mut files = vec![db.to_path_buf()];
    for suffix in ["-wal", "-shm"] {
        let mut s = db.as_os_str().to_owned();
        s.push(suffix);
        files.push(PathBuf::from(s));
    }
    files
}

/// Most bytes written for an archive entry on restore, so a crafted archive (a zip bomb) can't
/// fill the disk.
fn max_entry_bytes(entry: &str) -> u64 {
    match entry {
        DB_ENTRY => 8 * 1024 * 1024 * 1024,
        CONFIG_ENTRY => 1024 * 1024,
        _ => 64 * 1024,
    }
}

fn extract(archive: &mut zip::ZipArchive<File>, entry: &str, to: &Path) -> anyhow::Result<bool> {
    let limit = max_entry_bytes(entry);
    let file = match archive.by_name(entry) {
        Ok(f) => f,
        Err(zip::result::ZipError::FileNotFound) => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if let Some(parent) = to.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut out = File::create(to).with_context(|| format!("writing {}", to.display()))?;
    // One byte past the limit tells "exactly at the limit" apart from "too big".
    let written = std::io::copy(&mut file.take(limit + 1), &mut out)?;
    if written > limit {
        drop(out);
        let _ = std::fs::remove_file(to);
        bail!("{entry} in the backup is larger than {} KB", limit / 1024);
    }
    out.sync_all()?;
    Ok(true)
}

/// Settings that describe this installation rather than the backed-up data: a restored
/// `config.toml` keeps the current values (`None` = every key of the table).
const KEPT_CONFIG: &[(&str, Option<&str>)] = &[
    ("paths", None),
    ("database", None),
    ("security", Some("trusted_proxies")),
    ("security", Some("secret_key_file")),
    // The TMDB and TVDB addresses: a backup must not send this server's keys somewhere else.
    ("metadata", None),
];

/// Writes the backup's `config.toml` to `target`, keeping the [`KEPT_CONFIG`] keys of the
/// config currently there.
fn restore_config(archive: &mut zip::ZipArchive<File>, target: &Path) -> anyhow::Result<()> {
    let mut restored = String::new();
    archive
        .by_name(CONFIG_ENTRY)?
        .take(1024 * 1024)
        .read_to_string(&mut restored)
        .context("reading config.toml from the backup")?;
    let current = match std::fs::read_to_string(target) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", target.display())),
    };
    let (merged, kept) = merge_restored_config(current.as_deref(), &restored)?;
    if !kept.is_empty() {
        tracing::warn!(
            keys = %kept.join(", "),
            "restored config.toml: kept this installation's values for these keys"
        );
    }
    std::fs::write(target, merged).with_context(|| format!("writing {}", target.display()))
}

/// `restored` with every [`KEPT_CONFIG`] key taken from `current` (removed when `current`
/// doesn't set it), plus the `table.key` names that differed.
fn merge_restored_config(current: Option<&str>, restored: &str) -> anyhow::Result<(String, Vec<String>)> {
    let current: DocumentMut = current
        .unwrap_or_default()
        .parse()
        .context("the current config.toml is not valid TOML")?;
    let mut doc: DocumentMut = restored
        .parse()
        .context("config.toml in the backup is not valid TOML")?;
    let keys_of = |d: &DocumentMut, table: &str| -> Vec<String> {
        d.get(table)
            .and_then(Item::as_table_like)
            .map(|t| t.iter().map(|(k, _)| k.to_owned()).collect())
            .unwrap_or_default()
    };
    // Value text without surrounding whitespace/comments, for comparison.
    let value = |d: &DocumentMut, table: &str, key: &str| {
        d.get(table).and_then(|t| t.get(key)).map(|i| {
            let mut i = i.clone();
            if let Some(v) = i.as_value_mut() {
                v.decor_mut().clear();
            }
            i.to_string()
        })
    };
    let mut kept = Vec::new();
    for &(table, key) in KEPT_CONFIG {
        let keys = match key {
            Some(k) => vec![k.to_owned()],
            None => {
                let mut ks = keys_of(&current, table);
                for k in keys_of(&doc, table) {
                    if !ks.contains(&k) {
                        ks.push(k);
                    }
                }
                ks
            }
        };
        for key in keys {
            if value(&current, table, &key) == value(&doc, table, &key) {
                continue;
            }
            kept.push(format!("{table}.{key}"));
            let cur = current.get(table).and_then(|t| t.get(&key)).cloned();
            match (doc.get_mut(table).and_then(Item::as_table_like_mut), cur) {
                (Some(t), Some(item)) => {
                    t.insert(&key, item);
                }
                (Some(t), None) => {
                    t.remove(&key);
                }
                (None, Some(item)) => {
                    let mut t = Table::new();
                    t.insert(&key, item);
                    doc.insert(table, Item::Table(t));
                }
                (None, None) => {}
            }
        }
    }
    Ok((doc.to_string(), kept))
}

/// Moved-aside originals, so a failed restore can be undone.
struct Saved {
    dir: PathBuf,
    /// `(copy in dir, original location)`
    files: Vec<(PathBuf, PathBuf)>,
}

impl Saved {
    fn put_back(&self) {
        for (saved, original) in &self.files {
            let _ = std::fs::remove_file(original);
            if let Err(e) =
                std::fs::rename(saved, original).or_else(|_| std::fs::copy(saved, original).map(|_| ()))
            {
                tracing::error!(file = %original.display(), error = %e, "could not put back the original file");
            }
        }
    }
}

/// Applies a staged restore, if any. Runs at startup before the database is opened.
/// Renames `from` to `to`, falling back to copy + delete across filesystems. On Windows a
/// file that was just closed (SQLite's connection thread, an antivirus scan) can stay locked
/// for a few seconds (sqlx closes connections on its worker threads), so sharing violations
/// are retried for up to ten seconds.
fn move_aside(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        let res = std::fs::rename(from, to)
            .or_else(|_| std::fs::copy(from, to).and_then(|_| std::fs::remove_file(from)));
        match res {
            Err(e) if cfg!(windows) && attempt < 100 && e.raw_os_error() == Some(32) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            res => return res,
        }
    }
}

pub async fn apply_pending_restore(paths: &Paths, database_url: &str) -> anyhow::Result<bool> {
    let dir = restore_dir(paths);
    let pending = dir.join(PENDING_ZIP);
    if !pending.is_file() {
        return Ok(false);
    }
    let Some(db_file) = paths.db_file.clone() else {
        bail!("restoring backups is only supported for SQLite databases");
    };
    let source = self::pending(paths).map(|p| p.source).unwrap_or_default();
    tracing::warn!(%source, "applying the staged restore");

    let stamp = Utc::now().format(NAME_TIME).to_string();
    let saved_dir = dir.join(format!("pre-restore_{stamp}"));
    std::fs::create_dir_all(&saved_dir)?;
    let mut saved = Saved {
        dir: saved_dir.clone(),
        files: Vec::new(),
    };

    let result = (|| -> anyhow::Result<()> {
        // Move the live database aside; copy the config and key (they are only replaced if present).
        for f in db_files(&db_file) {
            if f.is_file() {
                let to = saved_dir.join(f.file_name().unwrap_or_default());
                move_aside(&f, &to).with_context(|| format!("moving {} aside", f.display()))?;
                saved.files.push((to, f));
            }
        }
        let mut archive = zip::ZipArchive::new(File::open(&pending)?)?;
        if !extract(&mut archive, DB_ENTRY, &db_file)? {
            bail!("the staged backup has no {DB_ENTRY}");
        }
        for (entry, target) in [
            (CONFIG_ENTRY, &paths.config_file),
            (SECRET_ENTRY, &paths.secret_key_file),
        ] {
            if entry == SECRET_ENTRY && !paths.backup_secret_key {
                continue;
            }
            if archive.by_name(entry).is_err() {
                continue;
            }
            if target.is_file() {
                let to = saved_dir.join(entry);
                std::fs::copy(target, &to)?;
                saved.files.push((to, target.clone()));
            }
            if entry == CONFIG_ENTRY {
                restore_config(&mut archive, target)?;
                continue;
            }
            // Pre-create the key owner-only; `extract` truncates it and keeps the permissions.
            create_private_file(target).with_context(|| format!("writing {}", target.display()))?;
            extract(&mut archive, entry, target)?;
        }
        Ok(())
    })();

    let result = match result {
        // Opening runs migrations, so a backup from an older version is upgraded here, and one
        // from a newer version (unknown migrations) is rejected before anything else uses it.
        Ok(()) => match reelix_db::Db::connect(database_url).await {
            Ok(db) => {
                db.pool().close().await;
                Ok(())
            }
            Err(e) => Err(anyhow::Error::new(e).context("the restored database could not be opened")),
        },
        Err(e) => Err(e),
    };

    match result {
        Ok(()) => {
            cancel(paths)?;
            tracing::warn!(
                %source,
                originals = %saved.dir.display(),
                "restore complete; a restored config.toml takes effect on the next restart"
            );
            Ok(true)
        }
        Err(e) => {
            for f in db_files(&db_file) {
                let _ = std::fs::remove_file(f);
            }
            saved.put_back();
            let failed = dir.join(format!("failed_{stamp}.zip"));
            let _ = std::fs::rename(&pending, &failed);
            let _ = std::fs::remove_file(dir.join(PENDING_INFO));
            Err(e.context(format!(
                "restore failed and was rolled back; the archive was kept as {}",
                failed.display()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &Path) -> Paths {
        Paths {
            data_dir: root.to_path_buf(),
            backup_dir: root.join("backups"),
            log_dir: root.join("logs"),
            db_file: Some(root.join("reelix.db")),
            config_file: root.join("config.toml"),
            secret_key_file: root.join("secret.key"),
            backup_secret_key: true,
        }
    }

    fn url(p: &Paths) -> String {
        format!("sqlite://{}", p.db_file.as_ref().unwrap().display())
    }

    async fn snapshot(db: &reelix_db::Db, to: &Path) {
        sqlx::query("VACUUM INTO ?")
            .bind(to.to_string_lossy().into_owned())
            .execute(db.pool())
            .await
            .unwrap();
    }

    #[test]
    fn names_are_parsed_and_validated() {
        let (kind, time) = parse_name("reelix_backup_scheduled_2026.10.30_21.04.11.zip").unwrap();
        assert_eq!(kind, BackupKind::Scheduled);
        assert_eq!(time.to_rfc3339(), "2026-10-30T21:04:11+00:00");
        assert!(parse_name("reelix_backup_manual_2026.10.30_21.04.11-2.zip").is_some());
        assert!(parse_name("other.zip").is_none());
        assert!(is_safe_name("reelix_backup_manual_2026.10.30_21.04.11.zip"));
        for bad in [
            "",
            "../x.zip",
            "a/b.zip",
            "a\\b.zip",
            ".hidden.zip",
            "a b.zip",
            "x\".zip",
        ] {
            assert!(!is_safe_name(bad), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn backup_list_prune_and_restore_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        std::fs::write(
            &p.config_file,
            "[server]\nport = 7701\n[paths]\ndata_dir = \"/old\"\n[security]\ntrusted_proxies = [\"10.0.0.1\"]\n",
        )
        .unwrap();
        std::fs::write(&p.secret_key_file, "old-key").unwrap();

        let db = reelix_db::Db::connect(&url(&p)).await.unwrap();
        db.settings().set("marker", "before").await.unwrap();
        let snap = tmp.path().join("snap.db");
        snapshot(&db, &snap).await;
        let t0 = Utc::now();
        let backup = write_archive(&p, &snap, BackupKind::Manual, t0).unwrap();
        validate(&backup).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&backup), 0o600);
            assert_eq!(mode(&p.backup_dir), 0o700);
        }

        // Two more scheduled backups; retention 1 keeps the newest and never touches manual ones.
        for secs in [1, 2] {
            write_archive(
                &p,
                &snap,
                BackupKind::Scheduled,
                t0 + chrono::Duration::seconds(secs),
            )
            .unwrap();
        }
        assert_eq!(list(&p).unwrap().len(), 3);
        assert_eq!(prune_scheduled(&p, 1).unwrap(), 1);
        let left = list(&p).unwrap();
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].kind, BackupKind::Scheduled);
        assert_eq!(left[1].kind, BackupKind::Manual);
        assert_eq!(left[1].version.as_deref(), Some(reelix_core::VERSION));

        // Change state after the backup, then restore it.
        db.settings().set("marker", "after").await.unwrap();
        db.pool().close().await;
        std::fs::write(
            &p.config_file,
            "[server]\nport = 9999\n[paths]\ndata_dir = \"/new\"\n",
        )
        .unwrap();
        std::fs::write(&p.secret_key_file, "new-key").unwrap();

        stage(&p, &backup, "manual", Utc::now()).unwrap();
        assert!(pending(&p).is_some());
        assert!(apply_pending_restore(&p, &url(&p)).await.unwrap());
        assert!(pending(&p).is_none());

        let db = reelix_db::Db::connect(&url(&p)).await.unwrap();
        assert_eq!(
            db.settings().get::<String>("marker").await.unwrap().as_deref(),
            Some("before")
        );
        // The backup's settings come back, but not its installation paths or proxies.
        let config = std::fs::read_to_string(&p.config_file).unwrap();
        assert!(config.contains("7701"), "{config}");
        assert!(
            config.contains("\"/new\"") && !config.contains("/old"),
            "{config}"
        );
        assert!(!config.contains("10.0.0.1"), "{config}");
        assert_eq!(std::fs::read_to_string(&p.secret_key_file).unwrap(), "old-key");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p.secret_key_file)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Nothing staged: nothing to do.
        assert!(!apply_pending_restore(&p, &url(&p)).await.unwrap());
    }

    #[test]
    fn restored_config_keeps_installation_keys() {
        let current = "[paths]\ndata_dir = \"/data\"\n[security]\nsecret_key_file = \"/data/k\"\n";
        let restored = "# from the backup\n[server]\nport = 1234\n[paths]\ndata_dir = \"/elsewhere\"\nlog_dir = \"/logs\"\n\n[database]\nurl = \"sqlite:///other.db\"\n\n[security]\ntrusted_proxies = [\"0.0.0.0/0\"]\n";
        let (merged, kept) = merge_restored_config(Some(current), restored).unwrap();
        assert_eq!(
            kept,
            [
                "paths.data_dir",
                "paths.log_dir",
                "database.url",
                "security.trusted_proxies",
                "security.secret_key_file"
            ]
        );
        let doc: DocumentMut = merged.parse().unwrap();
        assert!(merged.starts_with("# from the backup"), "{merged}");
        assert_eq!(doc["server"]["port"].as_integer(), Some(1234));
        assert_eq!(doc["paths"]["data_dir"].as_str(), Some("/data"));
        assert!(doc["paths"].get("log_dir").is_none());
        assert!(doc["database"].get("url").is_none());
        assert!(doc["security"].get("trusted_proxies").is_none());
        assert_eq!(doc["security"]["secret_key_file"].as_str(), Some("/data/k"));

        // A backup's TMDB address does not replace this server's.
        let ours = "[metadata]\ntmdb_base_url = \"https://api.themoviedb.org/3\"\n";
        let theirs = "[metadata]\ntmdb_base_url = \"http://elsewhere.example/3\"\n";
        let (merged, kept) = merge_restored_config(Some(ours), theirs).unwrap();
        assert_eq!(kept, ["metadata.tmdb_base_url"]);
        assert!(!merged.contains("elsewhere"), "{merged}");

        // Same values: nothing to report; no current config: the keys are dropped.
        let (_, kept) = merge_restored_config(Some(current), current).unwrap();
        assert!(kept.is_empty());
        let (merged, _) = merge_restored_config(None, restored).unwrap();
        assert!(
            !merged.contains("/elsewhere") && merged.contains("port = 1234"),
            "{merged}"
        );
    }

    #[tokio::test]
    async fn invalid_archives_are_rejected_and_bad_restores_roll_back() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        let junk = tmp.path().join("junk.zip");
        std::fs::write(&junk, b"not a zip").unwrap();
        assert!(stage(&p, &junk, "junk", Utc::now()).is_err());
        let upload = upload_path(&p).unwrap();
        std::fs::write(&upload, b"also not a zip").unwrap();
        assert!(stage_upload(&p, &upload, Utc::now()).is_err());
        assert!(pending(&p).is_none());

        // An entry over its cap is refused while extracting, and the restore rolls back.
        std::fs::write(&p.secret_key_file, "live-key").unwrap();
        let bomb = tmp.path().join("bomb.zip");
        {
            // A separate database: on Windows the live file can stay locked for a while after
            // its pool closes, which would make the restore below fail for the wrong reason.
            let other = tmp.path().join("bomb-source.db");
            let db = reelix_db::Db::connect(&format!("sqlite://{}", other.display()))
                .await
                .unwrap();
            let snap = tmp.path().join("bomb-snap.db");
            snapshot(&db, &snap).await;
            db.pool().close().await;
            let mut zip = zip::ZipWriter::new(File::create(&bomb).unwrap());
            zip.start_file(DB_ENTRY, SimpleFileOptions::default()).unwrap();
            zip.write_all(&std::fs::read(&snap).unwrap()).unwrap();
            zip.start_file(SECRET_ENTRY, SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&vec![b'k'; 1024 * 1024]).unwrap();
            zip.finish().unwrap();
        }
        let upload = upload_path(&p).unwrap();
        std::fs::copy(&bomb, &upload).unwrap();
        stage_upload(&p, &upload, Utc::now()).unwrap();
        assert!(!upload.exists());
        let err = apply_pending_restore(&p, &url(&p)).await.unwrap_err();
        assert!(format!("{err:#}").contains("larger than"), "{err:#}");
        assert_eq!(std::fs::read_to_string(&p.secret_key_file).unwrap(), "live-key");
        assert!(pending(&p).is_none());

        // A zip whose "database" is garbage after a valid header fails to open and is rolled back.
        let db = reelix_db::Db::connect(&url(&p)).await.unwrap();
        db.settings().set("marker", "live").await.unwrap();
        db.pool().close().await;
        let bad = tmp.path().join("bad.zip");
        {
            let mut zip = zip::ZipWriter::new(File::create(&bad).unwrap());
            zip.start_file(DB_ENTRY, SimpleFileOptions::default()).unwrap();
            zip.write_all(SQLITE_MAGIC).unwrap();
            zip.write_all(&[0xAB; 4096]).unwrap();
            zip.finish().unwrap();
        }
        stage(&p, &bad, "bad", Utc::now()).unwrap();
        assert!(apply_pending_restore(&p, &url(&p)).await.is_err());
        assert!(pending(&p).is_none());
        let db = reelix_db::Db::connect(&url(&p)).await.unwrap();
        assert_eq!(
            db.settings().get::<String>("marker").await.unwrap().as_deref(),
            Some("live")
        );
    }
}
