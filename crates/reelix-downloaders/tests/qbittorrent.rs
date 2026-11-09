//! qBittorrent adapter against recorded-style WebUI API v2 responses.

mod common;

use common::{build, json_fixture, settings_for};
use reelix_core::MediaKind;
use reelix_downloaders::torrent::{info_hash, sample_torrent};
use reelix_downloaders::*;
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SID: &str = "SID=0aBcD1eFg2HiJ3kLm4NoP5qRs6TuV7wX";

async fn mount_login(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .and(body_string_contains("username=admin"))
        .and(body_string_contains("password=adminadmin"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "set-cookie",
                    format!("{SID}; HttpOnly; SameSite=Strict; path=/").as_str(),
                )
                .set_body_string("Ok."),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Fails."))
        .mount(server)
        .await;
}

fn authed(m: &str, p: &str) -> wiremock::MockBuilder {
    Mock::given(method(m)).and(path(p)).and(header("cookie", SID))
}

async fn mount_common(server: &MockServer, api: &str) {
    mount_login(server).await;
    authed("GET", "/api/v2/app/webapiVersion")
        .respond_with(ResponseTemplate::new(200).set_body_string(api))
        .mount(server)
        .await;
    authed("GET", "/api/v2/app/version")
        .respond_with(ResponseTemplate::new(200).set_body_string("v4.6.7"))
        .mount(server)
        .await;
    authed("GET", "/api/v2/app/preferences")
        .respond_with(json_fixture("qbittorrent/preferences.json"))
        .mount(server)
        .await;
    authed("GET", "/api/v2/torrents/categories")
        .respond_with(json_fixture("qbittorrent/categories.json"))
        .mount(server)
        .await;
}

fn client(server: &MockServer) -> Box<dyn DownloadClient> {
    build(
        "qbittorrent",
        settings_for(server, json!({ "username": "admin", "password": "adminadmin" })),
    )
}

#[tokio::test]
async fn add_torrent_file_returns_info_hash() {
    let server = MockServer::start().await;
    mount_common(&server, "2.9.3").await;
    authed("POST", "/api/v2/torrents/add")
        .and(body_string_contains("name=\"category\"\r\n\r\ntv-reelix"))
        .and(body_string_contains("name=\"paused\"\r\n\r\ntrue"))
        .and(body_string_contains("name=\"ratioLimit\"\r\n\r\n1.5"))
        .and(body_string_contains("name=\"seedingTimeLimit\"\r\n\r\n4320"))
        .and(body_string_contains("filename=\"a.torrent\""))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    let bytes = sample_torrent("Andor.S02E06.mkv");
    let opts = AddOptions {
        media_kind: Some(MediaKind::Series),
        add_paused: true,
        seed_ratio: Some(1.5),
        seed_time_minutes: Some(4320),
        ..Default::default()
    };
    let id = c
        .add(
            &ReleasePayload::TorrentFile {
                bytes: bytes.clone(),
                name: "a.torrent".into(),
            },
            &opts,
        )
        .await
        .unwrap();
    assert_eq!(id.as_str(), info_hash(&bytes).unwrap());
}

#[tokio::test]
async fn add_magnet_on_qbit5_uses_stopped_and_creates_category() {
    let server = MockServer::start().await;
    mount_common(&server, "2.11.2").await;
    authed("POST", "/api/v2/torrents/createCategory")
        .and(body_string_contains("category=Anime"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    authed("POST", "/api/v2/torrents/add")
        .and(body_string_contains("name=\"stopped\"\r\n\r\ntrue"))
        .and(body_string_contains("name=\"urls\"\r\n\r\nmagnet:?xt=urn:btih:"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    let magnet = "magnet:?xt=urn:btih:C12FE1C06BBA254A9DC9F519B335AA7C1367A88A&dn=Frieren";
    let opts = AddOptions {
        category: Some("Anime".into()),
        priority: Some(Priority::Paused),
        ..Default::default()
    };
    let id = c
        .add(&ReleasePayload::Magnet(magnet.into()), &opts)
        .await
        .unwrap();
    assert_eq!(id.as_str(), "c12fe1c06bba254a9dc9f519b335aa7c1367a88a");
}

#[tokio::test]
async fn add_url_follows_redirect_to_magnet() {
    let server = MockServer::start().await;
    mount_common(&server, "2.9.3").await;
    let magnet = "magnet:?xt=urn:btih:YEX6DQDLXISUVHOJ6UM3GNNKPQJWPKEK";
    Mock::given(path("/dl/42"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", magnet))
        .mount(&server)
        .await;
    authed("POST", "/api/v2/torrents/add")
        .and(body_string_contains("YEX6DQDLXISUVHOJ6UM3GNNKPQJWPKEK"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    let id = c
        .add(
            &ReleasePayload::Url(format!("{}/dl/42", server.uri())),
            &AddOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(id.as_str(), "c12fe1c06bba254a9dc9f519b335aa7c1367a88a");
}

#[tokio::test]
async fn add_rejected_with_fails() {
    let server = MockServer::start().await;
    mount_common(&server, "2.9.3").await;
    authed("POST", "/api/v2/torrents/add")
        .respond_with(ResponseTemplate::new(200).set_body_string("Fails."))
        .mount(&server)
        .await;
    let c = client(&server);
    let err = c
        .add(
            &ReleasePayload::Magnet("magnet:?xt=urn:btih:C12FE1C06BBA254A9DC9F519B335AA7C1367A88A".into()),
            &AddOptions::default(),
        )
        .await;
    assert!(matches!(err, Err(ClientError::Rejected(_))));
}

#[tokio::test]
async fn items_map_states_and_seed_limits() {
    let server = MockServer::start().await;
    mount_common(&server, "2.9.3").await;
    authed("GET", "/api/v2/torrents/info")
        .and(query_param("category", "tv-reelix"))
        .respond_with(json_fixture("qbittorrent/torrents_tv.json"))
        .mount(&server)
        .await;
    authed("GET", "/api/v2/torrents/info")
        .and(query_param("category", "movies-reelix"))
        .respond_with(json_fixture("qbittorrent/torrents_empty.json"))
        .mount(&server)
        .await;
    let c = client(&server);
    let items = c.items().await.unwrap();
    assert_eq!(items.len(), 5);
    let s: Vec<_> = items.iter().map(|i| i.state).collect();
    use DownloadState::*;
    assert_eq!(s, [Completed, Completed, Downloading, Warning, Queued]);
    // pausedUP with ratio 2.04 >= global 2.0 → removable, id lowercased, content path used.
    assert!(items[0].can_be_removed && items[0].can_move_files);
    assert_eq!(
        items[0].download_id.as_str(),
        "8c212779b4abde7e8e7e2f7a4b9b9c6f2e1d0a11"
    );
    assert_eq!(
        items[0].output_path.as_deref(),
        Some("/downloads/tv/Andor.S02E01.1080p.WEB.H264-SuccessfulCrab.mkv")
    );
    assert_eq!(items[0].seeding_time, Some(7200));
    assert_eq!(items[0].completed_at, Some(1730101000));
    // Still seeding → not removable.
    assert!(!items[1].can_be_removed);
    assert_eq!(items[2].eta_secs, Some(133));
    assert!((items[2].progress() - 0.5).abs() < 1e-9);
    assert!(items[3].message.as_deref().unwrap().contains("stalled"));
    assert_eq!(items[4].message.as_deref(), Some("Downloading metadata"));
}

#[tokio::test]
async fn relogs_in_after_session_expiry() {
    let server = MockServer::start().await;
    mount_login(&server).await;
    // First call with the cookie gets 403 once (expired), then succeeds.
    authed("POST", "/api/v2/torrents/delete")
        .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    authed("POST", "/api/v2/torrents/delete")
        .and(body_string_contains("hashes=abc"))
        .and(body_string_contains("deleteFiles=true"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let c = client(&server);
    c.remove(&"abc".into(), true).await.unwrap();
}

#[tokio::test]
async fn pause_resume_priority_endpoints_by_version() {
    let server = MockServer::start().await;
    mount_common(&server, "2.11.2").await;
    for p in [
        "/api/v2/torrents/stop",
        "/api/v2/torrents/start",
        "/api/v2/torrents/setForceStart",
        "/api/v2/torrents/topPrio",
    ] {
        authed("POST", p)
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
    }
    let c = client(&server);
    let id: DownloadId = "abc".into();
    c.pause(&id).await.unwrap();
    c.resume(&id).await.unwrap();
    c.set_priority(&id, Priority::Force).await.unwrap();
    c.set_priority(&id, Priority::High).await.unwrap();
}

#[tokio::test]
async fn test_status_and_bad_credentials() {
    let server = MockServer::start().await;
    mount_common(&server, "2.9.3").await;
    let c = client(&server);
    c.test().await.unwrap();
    let st = c.status().await.unwrap();
    assert_eq!(st.version.as_deref(), Some("v4.6.7"));
    assert!(st.warnings.is_empty());
    let dirs: Vec<_> = st.output_dirs.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(dirs, ["/downloads", "/downloads/movies", "/downloads/tv-reelix"]);
    let mut cats = c.categories().await.unwrap();
    cats.sort();
    assert_eq!(cats, ["linux-isos", "movies-reelix", "tv-reelix"]);

    let bad = build(
        "qbittorrent",
        settings_for(&server, json!({ "username": "admin", "password": "wrong" })),
    );
    let r = bad.test_report().await;
    assert!(r.checks[0].ok && !r.checks[1].ok);
    assert!(matches!(bad.test().await, Err(ClientError::Auth(_))));
}

#[tokio::test]
async fn warns_when_ratio_action_removes_torrents() {
    let server = MockServer::start().await;
    mount_login(&server).await;
    authed("GET", "/api/v2/app/webapiVersion")
        .respond_with(ResponseTemplate::new(200).set_body_string("2.9.3"))
        .mount(&server)
        .await;
    authed("GET", "/api/v2/app/version")
        .respond_with(ResponseTemplate::new(200).set_body_string("v4.6.7"))
        .mount(&server)
        .await;
    authed("GET", "/api/v2/app/preferences")
        .respond_with(json_fixture("qbittorrent/preferences_remove.json"))
        .mount(&server)
        .await;
    authed("GET", "/api/v2/torrents/categories")
        .respond_with(json_fixture("qbittorrent/categories.json"))
        .mount(&server)
        .await;
    let c = client(&server);
    assert!(c.test().await.is_err());
    let st = c.status().await.unwrap();
    assert_eq!(st.warnings[0].kind, HealthKind::RemovesOnRatio);
}

#[tokio::test]
async fn items_include_the_book_category() {
    let server = MockServer::start().await;
    mount_common(&server, "2.9.3").await;
    for cat in ["movies-reelix", "tv-reelix"] {
        authed("GET", "/api/v2/torrents/info")
            .and(query_param("category", cat))
            .respond_with(json_fixture("qbittorrent/torrents_empty.json"))
            .mount(&server)
            .await;
    }
    authed("GET", "/api/v2/torrents/info")
        .and(query_param("category", "books-reelix"))
        .respond_with(json_fixture("qbittorrent/torrents_tv.json"))
        .expect(1)
        .mount(&server)
        .await;
    let c = build(
        "qbittorrent",
        settings_for(
            &server,
            json!({ "username": "admin", "password": "adminadmin", "book_category": "books-reelix" }),
        ),
    );
    assert_eq!(
        c.category_for_kind(reelix_core::ContentKind::Ebook).as_deref(),
        Some("books-reelix")
    );
    assert_eq!(c.items().await.unwrap().len(), 5);
}

/// qBittorrent 5.1 and later: the login answers 204 with no text and a cookie named QBT_SID_<port>.
#[tokio::test]
async fn signs_in_to_qbittorrent_5_1_with_its_port_named_cookie() {
    const NEW_SID: &str = "QBT_SID_8080=0aBcD1eFg2HiJ3kLm4NoP5qRs6TuV7wX";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .and(body_string_contains("password=adminadmin"))
        .respond_with(ResponseTemplate::new(204).insert_header(
            "set-cookie",
            format!("{NEW_SID}; HttpOnly; SameSite=Lax; path=/").as_str(),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/webapiVersion"))
        .and(header("cookie", NEW_SID))
        .respond_with(ResponseTemplate::new(200).set_body_string("2.11.4"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .and(header("cookie", NEW_SID))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.1.2"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/preferences"))
        .and(header("cookie", NEW_SID))
        .respond_with(json_fixture("qbittorrent/preferences.json"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/categories"))
        .and(header("cookie", NEW_SID))
        .respond_with(json_fixture("qbittorrent/categories.json"))
        .mount(&server)
        .await;
    let c = client(&server);
    c.test().await.expect("qBittorrent 5.1 sign-in works");
}
