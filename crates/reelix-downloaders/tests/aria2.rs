//! aria2 adapter against recorded-style JSON-RPC responses.

mod common;

use base64::Engine;
use common::{build, settings_for};
use reelix_core::MediaKind;
use reelix_core::provider::FieldKind;
use reelix_downloaders::torrent::{info_hash, sample_torrent};
use reelix_downloaders::*;
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RPC: &str = "/jsonrpc";

fn rpc_ok(result: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "jsonrpc": "2.0",
        "id": "reelix",
        "result": result,
    }))
}

fn rpc_err(code: i64, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "jsonrpc": "2.0",
        "id": "reelix",
        "error": { "code": code, "message": message },
    }))
}

async fn mount_version(server: &MockServer, version: &str) {
    Mock::given(method("POST"))
        .and(path(RPC))
        .and(body_string_contains("aria2.getVersion"))
        .respond_with(rpc_ok(json!({
            "version": version,
            "enabledFeatures": ["BitTorrent", "HTTPS", "XML-RPC"],
        })))
        .mount(server)
        .await;
}

async fn mount_options(server: &MockServer, max_download_result: &str) {
    Mock::given(method("POST"))
        .and(path(RPC))
        .and(body_string_contains("aria2.getGlobalOption"))
        .respond_with(rpc_ok(json!({
            "dir": "/downloads",
            "max-download-result": max_download_result,
            "bt-metadata-only": "false",
            "seed-ratio": "1.0",
        })))
        .mount(server)
        .await;
}

fn client(server: &MockServer, extra: Value) -> Box<dyn DownloadClient> {
    build("aria2", settings_for(server, extra))
}

#[tokio::test]
async fn add_magnet_sets_category_folder_and_seed_limits() {
    let server = MockServer::start().await;
    mount_options(&server, "1000").await;
    Mock::given(method("POST"))
        .and(path(RPC))
        .and(body_string_contains("aria2.addUri"))
        .and(body_string_contains("token:s3cret"))
        .and(body_string_contains("magnet:?xt=urn:btih:"))
        .and(body_string_contains("\"dir\":\"/downloads/tv-reelix\""))
        .and(body_string_contains("\"pause\":\"true\""))
        .and(body_string_contains("\"seed-ratio\":\"1.5\""))
        .and(body_string_contains("\"seed-time\":\"4320\""))
        .respond_with(rpc_ok(json!("0123456789abcdef")))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(
        &server,
        json!({
            "rpc_secret": "s3cret",
            "add_paused": true,
            "seed_ratio": 1.5,
            "seed_time_minutes": 4320,
        }),
    );
    let magnet = "magnet:?xt=urn:btih:C12FE1C06BBA254A9DC9F519B335AA7C1367A88A&dn=Andor";
    let id = c
        .add(
            &ReleasePayload::Magnet(magnet.into()),
            &AddOptions {
                media_kind: Some(MediaKind::Series),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(id.as_str(), "c12fe1c06bba254a9dc9f519b335aa7c1367a88a");
}

#[tokio::test]
async fn add_torrent_uploads_base64_and_uses_movie_folder() {
    let server = MockServer::start().await;
    mount_options(&server, "1000").await;
    let torrent = sample_torrent("Movie.2020.1080p.mkv");
    let expected = info_hash(&torrent).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&torrent);
    Mock::given(method("POST"))
        .and(path(RPC))
        .and(body_string_contains("aria2.addTorrent"))
        .and(body_string_contains(&encoded))
        .and(body_string_contains("\"dir\":\"/downloads/movies-reelix\""))
        .respond_with(rpc_ok(json!("fedcba9876543210")))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server, json!({}));
    let id = c
        .add(
            &ReleasePayload::TorrentFile {
                bytes: torrent,
                name: "movie.torrent".into(),
            },
            &AddOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(id.as_str(), expected);
}

#[tokio::test]
async fn add_rpc_error_is_rejected() {
    let server = MockServer::start().await;
    mount_options(&server, "1000").await;
    Mock::given(method("POST"))
        .and(path(RPC))
        .and(body_string_contains("aria2.addUri"))
        .respond_with(rpc_err(1, "Cannot add download: already exists"))
        .mount(&server)
        .await;
    let c = client(&server, json!({}));
    let err = c
        .add(
            &ReleasePayload::Magnet("magnet:?xt=urn:btih:C12FE1C06BBA254A9DC9F519B335AA7C1367A88A".into()),
            &AddOptions::default(),
        )
        .await;
    assert!(matches!(err, Err(ClientError::Rejected(_))));
}

#[tokio::test]
async fn items_only_include_category_folders_and_skip_metadata_shells() {
    let server = MockServer::start().await;
    mount_options(&server, "1000").await;

    let hash = |c: char| c.to_string().repeat(40);

    Mock::given(method("POST"))
        .and(path(RPC))
        .and(body_string_contains("aria2.tellActive"))
        .respond_with(rpc_ok(json!([
            {
                "gid": "1111111111111111",
                "status": "active",
                "infoHash": hash('1'),
                "totalLength": "1000",
                "completedLength": "400",
                "uploadLength": "50",
                "downloadSpeed": "100",
                "dir": "/downloads/tv-reelix",
                "files": [{
                    "path": "/downloads/tv-reelix/Show.S01E01.mkv",
                    "length": "1000",
                    "completedLength": "400"
                }]
            },
            {
                "gid": "2222222222222222",
                "status": "active",
                "infoHash": hash('2'),
                "seeder": "true",
                "totalLength": "500",
                "completedLength": "500",
                "uploadLength": "1000",
                "downloadSpeed": "0",
                "dir": "/downloads/movies-reelix",
                "bittorrent": {
                    "mode": "single",
                    "info": { "name": "Movie.2020.1080p.mkv" }
                },
                "files": [{
                    "path": "/downloads/movies-reelix/Movie.2020.1080p.mkv",
                    "length": "500",
                    "completedLength": "500"
                }]
            }
        ])))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(RPC))
        .and(body_string_contains("aria2.tellWaiting"))
        .respond_with(rpc_ok(json!([
            {
                "gid": "3333333333333333",
                "status": "paused",
                "infoHash": hash('3'),
                "totalLength": "200",
                "completedLength": "20",
                "dir": "/downloads/movies-reelix",
                "files": [{
                    "path": "/downloads/movies-reelix/other.mkv",
                    "length": "200",
                    "completedLength": "20"
                }]
            },
            {
                "gid": "4444444444444444",
                "status": "waiting",
                "infoHash": hash('4'),
                "totalLength": "10",
                "completedLength": "0",
                "dir": "/downloads/other",
                "files": [{ "path": "/downloads/other/skip.bin" }]
            }
        ])))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(RPC))
        .and(body_string_contains("aria2.tellStopped"))
        .respond_with(rpc_ok(json!([
            {
                "gid": "5555555555555555",
                "status": "complete",
                "infoHash": hash('5'),
                "totalLength": "3000",
                "completedLength": "3000",
                "uploadLength": "6000",
                "downloadSpeed": "0",
                "dir": "/downloads/tv-reelix",
                "bittorrent": {
                    "mode": "multi",
                    "info": { "name": "Show.S01.1080p" }
                },
                "files": [
                    {
                        "path": "/downloads/tv-reelix/Show.S01.1080p/E01.mkv",
                        "length": "1500",
                        "completedLength": "1500"
                    },
                    {
                        "path": "/downloads/tv-reelix/Show.S01.1080p/E02.mkv",
                        "length": "1500",
                        "completedLength": "1500"
                    }
                ]
            },
            {
                "gid": "6666666666666666",
                "status": "error",
                "infoHash": hash('6'),
                "errorCode": "9",
                "errorMessage": "Not enough disk space",
                "totalLength": "300",
                "completedLength": "10",
                "downloadSpeed": "0",
                "dir": "/downloads/movies-reelix",
                "files": [{ "path": "/downloads/movies-reelix/big.mkv" }]
            },
            {
                "gid": "7777777777777777",
                "status": "removed",
                "infoHash": hash('7'),
                "totalLength": "100",
                "completedLength": "100",
                "dir": "/downloads/tv-reelix"
            },
            {
                "gid": "9999999999999999",
                "status": "complete",
                "infoHash": hash('9'),
                "totalLength": "50",
                "completedLength": "50",
                "dir": "/other/tv-reelix",
                "files": [{ "path": "/other/tv-reelix/unrelated.mkv" }]
            },
            {
                "gid": "8888888888888888",
                "status": "complete",
                "infoHash": hash('5'),
                "followedBy": ["5555555555555555"],
                "totalLength": "20",
                "completedLength": "20",
                "downloadSpeed": "0",
                "dir": "/downloads/tv-reelix",
                "files": [{ "path": "/downloads/tv-reelix/[METADATA]aaaa.torrent" }]
            }
        ])))
        .mount(&server)
        .await;

    let c = client(&server, json!({}));
    let items = c.items().await.unwrap();
    let by_id = |id: &str| items.iter().find(|i| i.download_id.as_str() == id).unwrap();
    assert_eq!(items.len(), 5);
    assert!(items.iter().all(|i| i.download_id.as_str() != hash('9')));

    let downloading = by_id(&hash('1'));
    assert_eq!(downloading.state, DownloadState::Downloading);
    assert_eq!(downloading.category.as_deref(), Some("tv-reelix"));
    assert_eq!(downloading.remaining_bytes, 600);
    assert_eq!(downloading.eta_secs, Some(6));
    assert_eq!(
        downloading.output_path.as_deref(),
        Some("/downloads/tv-reelix/Show.S01E01.mkv")
    );
    assert!(!downloading.can_be_removed);

    let seeding = by_id(&hash('2'));
    assert_eq!(seeding.state, DownloadState::Completed);
    assert_eq!(seeding.title, "Movie.2020.1080p.mkv");
    assert!(!seeding.can_be_removed && !seeding.can_move_files);
    assert_eq!(seeding.seed_ratio, Some(2.0));

    assert_eq!(by_id(&hash('3')).state, DownloadState::Paused);
    assert!(items.iter().all(|i| i.download_id.as_str() != hash('4')));

    let complete = by_id(&hash('5'));
    assert_eq!(complete.state, DownloadState::Completed);
    assert_eq!(
        complete.output_path.as_deref(),
        Some("/downloads/tv-reelix/Show.S01.1080p")
    );
    assert!(complete.can_be_removed && complete.can_move_files);
    // The metadata shell for the same info hash is not a second item.
    assert_eq!(
        items
            .iter()
            .filter(|i| i.download_id.as_str() == hash('5'))
            .count(),
        1
    );

    let failed = by_id(&hash('6'));
    assert_eq!(failed.state, DownloadState::Failed);
    assert_eq!(failed.message.as_deref(), Some("Not enough disk space"));
    assert!(items.iter().all(|i| i.download_id.as_str() != hash('7')));
}

#[tokio::test]
async fn remove_pause_resume_and_priority_resolve_the_content_gid() {
    const INFO_HASH: &str = "0123456789abcdef0123456789abcdef01234567";
    const GID: &str = "0123456789abcdef";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(RPC))
        .and(body_string_contains("aria2.tellActive"))
        .respond_with(rpc_ok(json!([{
            "gid": GID,
            "status": "active",
            "infoHash": INFO_HASH,
            "following": "9999999999999999",
            "dir": "/downloads/tv-reelix"
        }])))
        .mount(&server)
        .await;
    for call in ["aria2.tellWaiting", "aria2.tellStopped"] {
        Mock::given(method("POST"))
            .and(path(RPC))
            .and(body_string_contains(call))
            .respond_with(rpc_ok(json!([])))
            .mount(&server)
            .await;
    }
    for call in [
        "aria2.forceRemove",
        "aria2.removeDownloadResult",
        "aria2.pause",
        "aria2.unpause",
    ] {
        Mock::given(method("POST"))
            .and(path(RPC))
            .and(body_string_contains(call))
            .and(body_string_contains(GID))
            .respond_with(rpc_ok(json!(GID)))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path(RPC))
        .and(body_string_contains("aria2.changePosition"))
        .and(body_string_contains("POS_SET"))
        .respond_with(rpc_ok(json!(0)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(RPC))
        .and(body_string_contains("aria2.changePosition"))
        .and(body_string_contains("POS_END"))
        .respond_with(rpc_ok(json!(3)))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server, json!({}));
    let id: DownloadId = INFO_HASH.into();
    c.remove(&id, true).await.unwrap();
    c.pause(&id).await.unwrap();
    c.resume(&id).await.unwrap();
    c.set_priority(&id, Priority::High).await.unwrap();
    c.set_priority(&id, Priority::Low).await.unwrap();
}

#[tokio::test]
async fn test_status_and_history_warning() {
    let server = MockServer::start().await;
    mount_version(&server, "1.37.0").await;
    mount_options(&server, "1000").await;
    let c = client(&server, json!({ "rpc_secret": "s3cret" }));
    c.test().await.unwrap();
    let st = c.status().await.unwrap();
    assert_eq!(st.version.as_deref(), Some("1.37.0"));
    assert!(st.warnings.is_empty());
    let dirs: Vec<_> = st.output_dirs.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(
        dirs,
        ["/downloads", "/downloads/movies-reelix", "/downloads/tv-reelix"]
    );
    assert_eq!(c.categories().await.unwrap(), ["movies-reelix", "tv-reelix"]);

    let server = MockServer::start().await;
    mount_version(&server, "1.37.0").await;
    mount_options(&server, "0").await;
    let c = client(&server, json!({}));
    assert!(matches!(c.test().await, Err(ClientError::InvalidSettings(_))));
    let st = c.status().await.unwrap();
    assert!(st.warnings.iter().any(|w| w.kind == HealthKind::HistoryRetention));
}

#[tokio::test]
async fn metadata_only_mode_is_a_health_warning() {
    let server = MockServer::start().await;
    mount_version(&server, "1.37.0").await;
    Mock::given(method("POST"))
        .and(path(RPC))
        .and(body_string_contains("aria2.getGlobalOption"))
        .respond_with(rpc_ok(json!({
            "dir": "/downloads",
            "max-download-result": "1000",
            "bt-metadata-only": "true",
        })))
        .mount(&server)
        .await;
    let c = client(&server, json!({}));
    assert!(matches!(c.test().await, Err(ClientError::InvalidSettings(_))));
    let st = c.status().await.unwrap();
    assert!(st.warnings.iter().any(|w| w.kind == HealthKind::Misconfigured));
}

#[test]
fn force_priority_is_not_offered_in_settings() {
    let schema = reelix_downloaders::aria2::schema();
    for name in ["recent_priority", "older_priority"] {
        let field = schema.fields.iter().find(|f| f.name == name).unwrap();
        let FieldKind::Select { options } = &field.kind else {
            panic!("{name} must be a select field");
        };
        assert!(
            options
                .iter()
                .all(|option| option.value.as_str() != Some("Force"))
        );
    }
}

#[tokio::test]
async fn force_priority_is_rejected_and_legacy_settings_are_reported() {
    let server = MockServer::start().await;
    let c = client(&server, json!({}));
    let magnet =
        ReleasePayload::Magnet("magnet:?xt=urn:btih:C12FE1C06BBA254A9DC9F519B335AA7C1367A88A".into());
    assert!(matches!(
        c.add(
            &magnet,
            &AddOptions {
                priority: Some(Priority::Force),
                ..Default::default()
            }
        )
        .await,
        Err(ClientError::Unsupported(_))
    ));
    assert!(matches!(
        c.set_priority(&"0123456789abcdef".into(), Priority::Force).await,
        Err(ClientError::Unsupported(_))
    ));

    mount_version(&server, "1.37.0").await;
    mount_options(&server, "1000").await;
    let c = client(&server, json!({ "recent_priority": "Force" }));
    let report = c.test_report().await;
    assert!(report.checks.last().is_some_and(|c| {
        !c.ok && c.name == "priority" && c.message.as_deref().unwrap_or("").contains("force-start")
    }));
    let status = c.status().await.unwrap();
    assert!(
        status
            .warnings
            .iter()
            .any(|w| { w.kind == HealthKind::Misconfigured && w.message.contains("force-start") })
    );
}

#[tokio::test]
async fn auth_failure_is_reported_as_auth_check() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(RPC))
        .and(body_string_contains("aria2.getVersion"))
        .respond_with(rpc_err(1, "Unauthorized"))
        .mount(&server)
        .await;
    let c = client(&server, json!({ "rpc_secret": "wrong" }));
    let report = c.test_report().await;
    assert!(report.checks[0].ok && report.checks[0].name == "connect");
    assert!(!report.checks[1].ok && report.checks[1].name == "auth");
    assert!(matches!(c.test().await, Err(ClientError::Auth(_))));
}
