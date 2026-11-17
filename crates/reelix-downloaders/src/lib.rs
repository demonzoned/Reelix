//! Download client adapters: aria2, qBittorrent, Transmission, Deluge, rTorrent, SABnzbd, NZBGet,
//! slskd, blackhole.
//!
//! Every adapter implements [`DownloadClient`], declares a [`ProviderSchema`] (see [`schemas`])
//! and is constructed from a validated settings object through [`build`]. Adapters only ever
//! read and touch downloads in Reelix's own categories.

pub mod aria2;
pub mod blackhole;
pub mod common;
pub mod deluge;
mod error;
pub mod model;
pub mod nzbget;
pub mod qbittorrent;
pub mod rtorrent;
pub mod sabnzbd;
pub mod slskd;
pub mod torrent;
pub mod transmission;
pub mod xmlrpc;

use async_trait::async_trait;
use reelix_core::provider::ProviderSchema;
use reelix_core::{ContentKind, Protocol};

pub use error::ClientError;
pub use model::{
    AddOptions, ClientInfo, ClientStatus, DownloadId, DownloadItem, DownloadState, FailureAction, HealthKind,
    HealthWarning, OutputDir, PayloadPreference, Priority, ReleasePayload, TestCheck, TestReport,
    UsenetFailure,
};

/// Shared resources handed to adapters at construction.
#[derive(Debug, Clone, Default)]
pub struct BuildContext {
    pub http: reqwest::Client,
    /// Persisted `download_clients.id`, if any (used in logs).
    pub instance_id: Option<i64>,
}

impl BuildContext {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            instance_id: None,
        }
    }
}

/// A download client (README "Key Traits").
#[async_trait]
pub trait DownloadClient: Send + Sync {
    /// Torrent, Usenet or Soulseek.
    fn protocol(&self) -> Protocol;

    /// Name, priority, removal flags and tags from the settings.
    fn info(&self) -> &ClientInfo;

    /// Whether the pipeline should fetch the file itself or pass the URL through.
    fn payload_preference(&self) -> PayloadPreference {
        PayloadPreference::File
    }

    /// Starts a download; returns the id used to track it.
    async fn add(&self, release: &ReleasePayload, opts: &AddOptions) -> Result<DownloadId, ClientError>;

    /// Current downloads in Reelix's categories (queue + history for Usenet).
    async fn items(&self) -> Result<Vec<DownloadItem>, ClientError>;

    /// Removes a download, optionally deleting its data.
    async fn remove(&self, id: &DownloadId, delete_data: bool) -> Result<(), ClientError>;

    /// Connection test; succeeds only if every check in [`Self::test_report`] passes.
    async fn test(&self) -> Result<(), ClientError>;

    /// Individual test checks (connect, auth, version, categories…) for the settings form.
    async fn test_report(&self) -> TestReport {
        let mut r = TestReport::default();
        match self.test().await {
            Ok(()) => r.pass("connection"),
            Err(e) => r.fail("connection", e.to_string()),
        }
        r
    }

    /// Version, output folders and health warnings.
    async fn status(&self) -> Result<ClientStatus, ClientError>;

    /// The configured category for a content kind (book/music fall back to the movie category),
    /// for the pipeline to pass as [`AddOptions::category`]. `None` = client default.
    fn category_for_kind(&self, _kind: ContentKind) -> Option<String> {
        None
    }

    /// Categories/labels known to the client (fills category dropdowns).
    async fn categories(&self) -> Result<Vec<String>, ClientError> {
        Ok(Vec::new())
    }

    async fn set_priority(&self, _id: &DownloadId, _priority: Priority) -> Result<(), ClientError> {
        Err(ClientError::Unsupported("changing priority"))
    }

    async fn pause(&self, _id: &DownloadId) -> Result<(), ClientError> {
        Err(ClientError::Unsupported("pausing downloads"))
    }

    async fn resume(&self, _id: &DownloadId) -> Result<(), ClientError> {
        Err(ClientError::Unsupported("resuming downloads"))
    }
}

/// Implementation ids accepted by [`build`].
pub const IMPLEMENTATIONS: [&str; 10] = [
    sabnzbd::IMPLEMENTATION,
    nzbget::IMPLEMENTATION,
    qbittorrent::IMPLEMENTATION,
    transmission::IMPLEMENTATION,
    deluge::IMPLEMENTATION,
    rtorrent::IMPLEMENTATION,
    aria2::IMPLEMENTATION,
    blackhole::TORRENT_IMPLEMENTATION,
    blackhole::USENET_IMPLEMENTATION,
    slskd::IMPLEMENTATION,
];

/// One settings schema per implementation.
pub fn schemas() -> Vec<ProviderSchema> {
    vec![
        sabnzbd::schema(),
        nzbget::schema(),
        qbittorrent::schema(),
        transmission::schema(),
        deluge::schema(),
        rtorrent::schema(),
        aria2::schema(),
        blackhole::torrent_schema(),
        blackhole::usenet_schema(),
        slskd::schema(),
    ]
}

/// Schema for one implementation.
pub fn schema(implementation: &str) -> Option<ProviderSchema> {
    schemas().into_iter().find(|s| s.implementation == implementation)
}

/// Protocol of an implementation without building it.
pub fn protocol_of(implementation: &str) -> Option<Protocol> {
    match implementation {
        sabnzbd::IMPLEMENTATION | nzbget::IMPLEMENTATION | blackhole::USENET_IMPLEMENTATION => {
            Some(Protocol::Usenet)
        }
        qbittorrent::IMPLEMENTATION
        | transmission::IMPLEMENTATION
        | deluge::IMPLEMENTATION
        | rtorrent::IMPLEMENTATION
        | aria2::IMPLEMENTATION
        | blackhole::TORRENT_IMPLEMENTATION => Some(Protocol::Torrent),
        slskd::IMPLEMENTATION => Some(Protocol::Soulseek),
        _ => None,
    }
}

/// Builds an adapter from its implementation id and settings (validated against its schema).
pub fn build(
    implementation: &str,
    settings: &serde_json::Value,
    ctx: &BuildContext,
) -> Result<Box<dyn DownloadClient>, ClientError> {
    Ok(match implementation {
        sabnzbd::IMPLEMENTATION => Box::new(sabnzbd::Sabnzbd::from_settings(settings, ctx)?),
        nzbget::IMPLEMENTATION => Box::new(nzbget::NzbGet::from_settings(settings, ctx)?),
        qbittorrent::IMPLEMENTATION => Box::new(qbittorrent::QBittorrent::from_settings(settings, ctx)?),
        transmission::IMPLEMENTATION => Box::new(transmission::Transmission::from_settings(settings, ctx)?),
        deluge::IMPLEMENTATION => Box::new(deluge::Deluge::from_settings(settings, ctx)?),
        rtorrent::IMPLEMENTATION => Box::new(rtorrent::RTorrent::from_settings(settings, ctx)?),
        aria2::IMPLEMENTATION => Box::new(aria2::Aria2::from_settings(settings, ctx)?),
        blackhole::TORRENT_IMPLEMENTATION => Box::new(blackhole::Blackhole::from_settings(
            Protocol::Torrent,
            settings,
            ctx,
        )?),
        blackhole::USENET_IMPLEMENTATION => Box::new(blackhole::Blackhole::from_settings(
            Protocol::Usenet,
            settings,
            ctx,
        )?),
        slskd::IMPLEMENTATION => Box::new(slskd::Slskd::from_settings(settings, ctx)?),
        other => return Err(ClientError::UnknownImplementation(other.into())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_are_unique_and_buildable_ids() {
        let s = schemas();
        assert_eq!(s.len(), IMPLEMENTATIONS.len());
        for (schema, id) in s.iter().zip(IMPLEMENTATIONS) {
            assert_eq!(schema.implementation, id);
            assert!(protocol_of(id).is_some());
            assert!(schema.fields.iter().any(|f| f.name == "name"));
        }
        assert!(matches!(
            build("nope", &serde_json::json!({}), &BuildContext::default()),
            Err(ClientError::UnknownImplementation(_))
        ));
    }
}
