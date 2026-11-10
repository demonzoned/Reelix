//! RAR unpacking with the real `unrar`. The archives in `fixtures/rar` hold one 6,000-byte
//! `Some.Movie.2020.1080p.WEB-DL.x264-GRP.mkv` (random bytes, stored) unless noted, and were made
//! with RAR 7.01:
//!
//! - `single.rar`: one volume.
//! - `split.part1.rar` to `split.part3.rar`: `-v2500b`.
//! - `oldsplit.rar`, `oldsplit.r00`, `oldsplit.r01`: the same set, renamed to the old naming.
//! - `novideo.rar`: only `readme.txt`.
//! - `password.rar` (`-p`) and `password-headers.rar` (`-hp`), password `Reelix`.
//! - `corrupt.rar`: `single.rar` with 16 bytes of the video overwritten.
//!
//! Without `unrar` on PATH these tests pass without checking anything, unless
//! `REELIX_REQUIRE_UNRAR` is set (as in CI on Linux).

use std::fs;
use std::path::{Path, PathBuf};

use reelix_import::unpack::{
    STAGING_DIR, UnpackConfig, find_archives, is_first_volume, remove_unpacked, unpack_download,
};

const VIDEO: &str = "Some.Movie.2020.1080p.WEB-DL.x264-GRP.mkv";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rar")
}

/// A download folder holding copies of the named fixtures.
fn download(names: &[&str]) -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let dl = fs::canonicalize(d.path())
        .unwrap()
        .join("Some.Movie.2020.1080p.WEB-DL.x264-GRP");
    fs::create_dir_all(&dl).unwrap();
    for n in names {
        fs::copy(fixtures().join(n), dl.join(n)).unwrap();
    }
    (d, dl)
}

fn unrar_available() -> bool {
    let found = std::process::Command::new("unrar")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok();
    if !found {
        assert!(
            std::env::var_os("REELIX_REQUIRE_UNRAR").is_none(),
            "REELIX_REQUIRE_UNRAR is set but unrar is not on PATH"
        );
        eprintln!("unrar is not on PATH; skipping");
    }
    found
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// The archives are still there, the video is next to them and nothing is left in staging.
fn assert_unpacked(dl: &Path, archives: &[&str]) {
    let video = dl.join(VIDEO);
    assert_eq!(fs::metadata(&video).unwrap().len(), 6000);
    for a in archives {
        assert!(dl.join(a).is_file(), "{a} was removed");
    }
    assert!(!dl.join(STAGING_DIR).exists());
}

#[test]
fn first_volumes() {
    for first in [
        "a.rar",
        "A.RAR",
        "a.part1.rar",
        "a.part01.rar",
        "a.part001.rar",
        "a.partial.rar",
    ] {
        assert!(is_first_volume(Path::new(first)), "{first}");
    }
    for later in [
        "a.part2.rar",
        "a.part02.rar",
        "a.part10.rar",
        "a.r00",
        "a.r01",
        "a.mkv",
        "rar",
    ] {
        assert!(!is_first_volume(Path::new(later)), "{later}");
    }
}

#[test]
fn find_archives_skips_later_volumes_and_unpack_folders() {
    let (_d, dl) = download(&["single.rar", "split.part1.rar", "split.part2.rar", "oldsplit.r00"]);
    fs::create_dir_all(dl.join("_UNPACK_x")).unwrap();
    fs::copy(fixtures().join("single.rar"), dl.join("_UNPACK_x/single.rar")).unwrap();
    fs::create_dir_all(dl.join("Subs")).unwrap();
    fs::copy(fixtures().join("novideo.rar"), dl.join("Subs/subs.rar")).unwrap();
    let found: Vec<PathBuf> = find_archives(&dl)
        .into_iter()
        .map(|p| p.strip_prefix(&dl).unwrap().to_path_buf())
        .collect();
    assert_eq!(
        found,
        [
            PathBuf::from("Subs/subs.rar"),
            PathBuf::from("single.rar"),
            PathBuf::from("split.part1.rar"),
        ]
    );
}

#[tokio::test]
async fn single_archive_is_unpacked_next_to_it() {
    if !unrar_available() {
        return;
    }
    let (_d, dl) = download(&["single.rar"]);
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.messages.is_empty(), "{:?}", u.messages);
    assert_eq!(u.files, [dl.join(VIDEO)]);
    assert_unpacked(&dl, &["single.rar"]);

    remove_unpacked(&u.files).await;
    assert_eq!(names(&dl), ["single.rar"]);
}

#[tokio::test]
async fn split_set_is_unpacked_once() {
    if !unrar_available() {
        return;
    }
    let parts = ["split.part1.rar", "split.part2.rar", "split.part3.rar"];
    let (_d, dl) = download(&parts);
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.messages.is_empty(), "{:?}", u.messages);
    assert_eq!(u.files, [dl.join(VIDEO)]);
    assert_unpacked(&dl, &parts);
    let single = {
        let (_d2, dl2) = download(&["single.rar"]);
        unpack_download(&dl2, &UnpackConfig::default()).await;
        fs::read(dl2.join(VIDEO)).unwrap()
    };
    assert_eq!(fs::read(dl.join(VIDEO)).unwrap(), single);
}

#[tokio::test]
async fn old_style_split_set_is_unpacked() {
    if !unrar_available() {
        return;
    }
    let parts = ["oldsplit.rar", "oldsplit.r00", "oldsplit.r01"];
    let (_d, dl) = download(&parts);
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.messages.is_empty(), "{:?}", u.messages);
    assert_eq!(u.files, [dl.join(VIDEO)]);
    assert_unpacked(&dl, &parts);
}

#[tokio::test]
async fn archive_without_video_is_left_packed() {
    if !unrar_available() {
        return;
    }
    let (_d, dl) = download(&["novideo.rar"]);
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.files.is_empty());
    assert_eq!(
        u.messages,
        ["The RAR archives in the download hold no video files"]
    );
    assert_eq!(names(&dl), ["novideo.rar"]);
}

#[tokio::test]
async fn password_protected_archives_say_so() {
    if !unrar_available() {
        return;
    }
    for archive in ["password.rar", "password-headers.rar"] {
        let (_d, dl) = download(&[archive]);
        let u = unpack_download(&dl, &UnpackConfig::default()).await;
        assert!(u.files.is_empty(), "{archive}");
        assert_eq!(
            u.messages,
            [format!("{archive}: the archive is password protected")]
        );
        assert_eq!(names(&dl), [archive]);
    }
}

#[tokio::test]
async fn damaged_archive_says_so_and_leaves_no_partial_video() {
    if !unrar_available() {
        return;
    }
    let (_d, dl) = download(&["corrupt.rar"]);
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.files.is_empty());
    assert_eq!(u.messages, ["corrupt.rar: the archive is damaged or incomplete"]);
    assert_eq!(names(&dl), ["corrupt.rar"]);
}

#[tokio::test]
async fn missing_volume_says_so() {
    if !unrar_available() {
        return;
    }
    let (_d, dl) = download(&["split.part1.rar", "split.part3.rar"]);
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.files.is_empty());
    assert_eq!(
        u.messages,
        ["split.part1.rar: the archive is damaged or incomplete"]
    );
    assert!(!dl.join(VIDEO).exists());
}

#[tokio::test]
async fn existing_files_are_not_replaced() {
    if !unrar_available() {
        return;
    }
    let (_d, dl) = download(&["single.rar"]);
    fs::write(dl.join(VIDEO), b"already here").unwrap();
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.files.is_empty());
    assert_eq!(fs::read(dl.join(VIDEO)).unwrap(), b"already here");
    assert!(!dl.join(STAGING_DIR).exists());
}

#[tokio::test]
async fn missing_unrar_says_so() {
    let (_d, dl) = download(&["single.rar", "split.part1.rar"]);
    let config = UnpackConfig {
        unrar_path: dl.join("no-such-unrar"),
        ..UnpackConfig::default()
    };
    let u = unpack_download(&dl, &config).await;
    assert!(u.files.is_empty());
    assert_eq!(
        u.messages,
        ["unrar is not installed, so the RAR archives cannot be unpacked"]
    );
}

#[tokio::test]
async fn folder_without_archives_does_nothing() {
    let (_d, dl) = download(&[]);
    fs::write(dl.join("readme.txt"), b"x").unwrap();
    let u = unpack_download(&dl, &UnpackConfig::default()).await;
    assert!(u.files.is_empty() && u.messages.is_empty());
}
