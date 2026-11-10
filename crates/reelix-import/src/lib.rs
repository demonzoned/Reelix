//! Media info (ffprobe), naming engine, file matching and safe filesystem operations.
//!
//! - [`mediainfo`]: run `ffprobe` and map its JSON to [`mediainfo::MediaInfo`].
//! - [`naming`]: Sonarr/Radarr-compatible token templates for folders and files.
//! - [`files`]: scanning, sample/extra detection, [`files::PathGuard`], transfers
//!   (hardlink / copy / move), recycle bin, permissions and free space.
//! - [`matching`]: match parsed releases to expected movies and episodes.
//! - [`content`]: finished downloads handed to the books and music importers.
//! - [`unpack`]: RAR archives in finished downloads, unpacked with `unrar`.

pub mod content;
pub mod error;
pub mod files;
pub mod matching;
pub mod mediainfo;
pub mod naming;
pub mod unpack;

pub use error::{Error, Result};

use serde::{Deserialize, Serialize};

/// How a series numbers its episodes; drives naming and episode matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriesType {
    #[default]
    Standard,
    /// Episodes identified by air date (talk shows, news).
    Daily,
    /// Absolute numbering (with optional season/episode).
    Anime,
}
