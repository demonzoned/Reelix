//! RAR archives in finished downloads: find the first volume of each set, list it and unpack it
//! with `unrar`.
//!
//! Files are unpacked into a hidden staging folder next to the archive ([`STAGING_DIR`]) and moved
//! into the archive's folder only when `unrar` succeeds, so a half-unpacked video is never seen by
//! an import (scans skip folders whose names start with `.`). The archives themselves are left
//! alone for the download client to seed and remove.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::files::is_video;

/// Hidden folder, next to the archive, that `unrar` writes into.
pub const STAGING_DIR: &str = ".reelix-unpack";

/// How deep below the download folder archives are looked for (season packs keep one set per
/// episode folder).
const MAX_DEPTH: usize = 3;

/// How to run `unrar`.
#[derive(Debug, Clone)]
pub struct UnpackConfig {
    /// `unrar` binary (default `unrar` from PATH).
    pub unrar_path: PathBuf,
    /// Limit for one archive set, after which `unrar` is stopped.
    pub timeout: Duration,
}

impl Default for UnpackConfig {
    fn default() -> Self {
        Self {
            unrar_path: PathBuf::from("unrar"),
            timeout: Duration::from_secs(2 * 60 * 60),
        }
    }
}

/// Why an archive could not be listed or unpacked. The messages are shown in the queue.
#[derive(Debug, thiserror::Error)]
pub enum UnpackError {
    #[error("unrar is not installed, so the RAR archives cannot be unpacked")]
    NotInstalled,
    #[error("the archive is password protected")]
    Encrypted,
    #[error("the archive is damaged or incomplete")]
    Damaged,
    #[error("unrar stopped after {0:?}")]
    Timeout(Duration),
    #[error("unrar failed (exit {code:?}): {stderr}")]
    Failed { code: Option<i32>, stderr: String },
    #[error("i/o error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl UnpackError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Whether `path` is the volume `unrar` should be given: a `.rar` file that is not a later part
/// of a `.partN.rar` set. Old-style sets (`.rar`, `.r00`, `.r01` and so on) start at the `.rar`.
pub fn is_first_volume(path: &Path) -> bool {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()) else {
        return false;
    };
    let Some(stem) = name.strip_suffix(".rar") else {
        return false;
    };
    match stem.rsplit_once(".part") {
        Some((_, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            n.trim_start_matches('0') == "1"
        }
        _ => true,
    }
}

/// First volumes of the RAR sets below `dir`, sorted. Hidden folders and the `_UNPACK_` /
/// `_FAILED_` folders of Usenet clients are skipped.
pub fn find_archives(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                if depth < MAX_DEPTH
                    && !name.starts_with('.')
                    && !name.starts_with("_unpack")
                    && !name.starts_with("_failed")
                {
                    walk(&path, depth + 1, out);
                }
            } else if ft.is_file() && is_first_volume(&path) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, 0, &mut out);
    out.sort();
    out
}

/// Paths inside `archive` (relative, folders included), read with `unrar lb`.
pub async fn list(archive: &Path, config: &UnpackConfig) -> Result<Vec<PathBuf>, UnpackError> {
    let out = run(config, &["lb", "-p-", "--"], archive, None).await?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect())
}

/// Whether `archive` holds at least one video file.
pub async fn has_video(archive: &Path, config: &UnpackConfig) -> Result<bool, UnpackError> {
    Ok(list(archive, config).await?.iter().any(|p| is_video(p)))
}

/// Unpacks `archive` (all of its volumes) into the folder it is in. Returns the files that were
/// added; a file that already exists there is kept and not replaced.
pub async fn unpack(archive: &Path, config: &UnpackConfig) -> Result<Vec<PathBuf>, UnpackError> {
    let dir = archive
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let stage = dir.join(STAGING_DIR);
    // A staging folder left by an earlier run (Reelix stopped part-way) holds nothing usable.
    remove_staging(&stage).await?;
    tokio::fs::create_dir_all(&stage)
        .await
        .map_err(|e| UnpackError::io(&stage, e))?;
    let result = run(
        config,
        &["x", "-o+", "-p-", "-y", "-idq", "--"],
        archive,
        Some(&stage),
    )
    .await;
    let moved = match result {
        Ok(_) => {
            let (stage2, dir2) = (stage.clone(), dir.clone());
            tokio::task::spawn_blocking(move || move_out(&stage2, &dir2))
                .await
                .unwrap_or_else(|e| Err(UnpackError::io(&stage, std::io::Error::other(e))))
        }
        Err(e) => Err(e),
    };
    remove_staging(&stage).await?;
    moved
}

/// What [`unpack_download`] did: the files it added and why any archive was not unpacked.
#[derive(Debug, Default)]
pub struct Unpacked {
    pub files: Vec<PathBuf>,
    pub messages: Vec<String>,
}

/// Unpacks every RAR set below `dir` that holds a video. Archives without a video (subtitles,
/// covers) are left packed. Each archive that cannot be unpacked gets a message naming it.
pub async fn unpack_download(dir: &Path, config: &UnpackConfig) -> Unpacked {
    let root = dir.to_path_buf();
    let archives = tokio::task::spawn_blocking(move || find_archives(&root))
        .await
        .unwrap_or_default();
    let mut done = Unpacked::default();
    if archives.is_empty() {
        return done;
    }
    let mut without_video = 0;
    for archive in &archives {
        let name = archive.strip_prefix(dir).unwrap_or(archive).display().to_string();
        let result = match has_video(archive, config).await {
            Ok(false) => {
                without_video += 1;
                continue;
            }
            Ok(true) => unpack(archive, config).await,
            Err(e) => Err(e),
        };
        match result {
            Ok(files) => {
                tracing::info!(archive = %archive.display(), files = files.len(), "unpacked");
                done.files.extend(files);
            }
            Err(UnpackError::NotInstalled) => {
                done.messages.push(UnpackError::NotInstalled.to_string());
                break;
            }
            Err(e) => {
                tracing::warn!(archive = %archive.display(), error = %e, "could not unpack");
                done.messages.push(format!("{name}: {e}"));
            }
        }
    }
    if done.files.is_empty() && done.messages.is_empty() && without_video > 0 {
        done.messages
            .push("The RAR archives in the download hold no video files".into());
    }
    done
}

/// Removes files that [`unpack_download`] added, once they have been imported. Files that are
/// gone already (moved by the import) are skipped.
pub async fn remove_unpacked(files: &[PathBuf]) {
    for f in files {
        match tokio::fs::remove_file(f).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(path = %f.display(), error = %e, "could not remove an unpacked file"),
        }
    }
}

async fn remove_staging(stage: &Path) -> Result<(), UnpackError> {
    match tokio::fs::remove_dir_all(stage).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(UnpackError::io(stage, e)),
    }
}

/// Moves every file below `stage` to the same place below `dir` (one file system, so a rename).
fn move_out(stage: &Path, dir: &Path) -> Result<Vec<PathBuf>, UnpackError> {
    fn walk(from: &Path, to: &Path, out: &mut Vec<PathBuf>) -> Result<(), UnpackError> {
        let entries = std::fs::read_dir(from).map_err(|e| UnpackError::io(from, e))?;
        for e in entries {
            let e = e.map_err(|e| UnpackError::io(from, e))?;
            let src = e.path();
            let dst = to.join(e.file_name());
            let ft = e.file_type().map_err(|e| UnpackError::io(&src, e))?;
            if ft.is_dir() {
                std::fs::create_dir_all(&dst).map_err(|e| UnpackError::io(&dst, e))?;
                walk(&src, &dst, out)?;
            } else if ft.is_file() && !dst.exists() {
                std::fs::rename(&src, &dst).map_err(|e| UnpackError::io(&dst, e))?;
                out.push(dst);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(stage, dir, &mut out)?;
    out.sort();
    Ok(out)
}

async fn run(
    config: &UnpackConfig,
    args: &[&str],
    archive: &Path,
    dest: Option<&Path>,
) -> Result<Vec<u8>, UnpackError> {
    let mut cmd = tokio::process::Command::new(&config.unrar_path);
    cmd.args(args).arg(archive);
    if let Some(d) = dest {
        // The trailing separator tells unrar that this is the destination folder.
        let mut d = d.as_os_str().to_owned();
        d.push(std::path::MAIN_SEPARATOR_STR);
        cmd.arg(d);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            UnpackError::NotInstalled
        } else {
            UnpackError::io(&config.unrar_path, e)
        }
    })?;
    let output = tokio::time::timeout(config.timeout, child.wait_with_output())
        .await
        .map_err(|_| UnpackError::Timeout(config.timeout))?
        .map_err(|e| UnpackError::io(&config.unrar_path, e))?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    match output.status.code() {
        // 1 is a warning that still unpacked everything.
        Some(0 | 1) => Ok(output.stdout),
        Some(11) => Err(UnpackError::Encrypted),
        _ if stderr.to_ascii_lowercase().contains("password") => Err(UnpackError::Encrypted),
        // 3: checksum error; 2: fatal error; 10: missing volume or nothing to unpack.
        Some(2 | 3 | 10) => Err(UnpackError::Damaged),
        code => Err(UnpackError::Failed { code, stderr }),
    }
}
