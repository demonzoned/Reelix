mod common;

use common::{ctx, fixture};
use reelix_core::Protocol;
use reelix_core::provider::FieldKind;
use reelix_indexers::{
    IndexerError, IndexerFlag, ProxyConfig, ProxyKind, SearchParam, SearchQuery, build, schemas,
};
use serde_json::json;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn xml(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body, "application/rss+xml")
}

async fn caps(server: &MockServer, file: &str) {
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "caps"))
        .respond_with(xml(fixture(file)))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn torznab_caps_and_tv_search() {
    let server = MockServer::start().await;
    caps(&server, "torznab_caps.xml").await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "tvsearch"))
        .and(query_param("tvdbid", "398811"))
        .and(query_param("season", "2"))
        .and(query_param("ep", "5"))
        .and(query_param("q", "Copper Valley"))
        .and(query_param("cat", "5000"))
        .and(query_param("apikey", "secret"))
        .and(query_param("limit", "50"))
        .respond_with(xml(fixture("torznab_search.xml")))
        .mount(&server)
        .await;

    let idx = build(
        "torznab",
        &json!({"base_url": server.uri(), "api_key": "secret", "categories": [5000]}),
        &ctx(3, "Proxy"),
    )
    .unwrap();
    assert_eq!(idx.protocol(), Protocol::Torrent);
    let c = idx.capabilities().await.unwrap();
    assert!(c.tv_search.supports(SearchParam::TvdbId) && c.movie_search.supports(SearchParam::TmdbId));
    assert_eq!((c.limit_default, c.limit_max), (Some(50), Some(200)));
    assert!(c.category_ids().contains(&100417));

    let q = SearchQuery {
        term: Some("Copper Valley".into()),
        tvdb_id: Some(398811),
        season: Some(2),
        episode: Some("5".into()),
        ..SearchQuery::tv()
    };
    let r = idx.search(&q).await.unwrap();
    assert_eq!(r.len(), 2);
    let a = &r[0];
    assert_eq!(a.indexer_id, 3);
    assert_eq!(a.indexer, "Proxy");
    assert_eq!(a.guid, "https://tracker.example/details/88123");
    assert_eq!(
        a.info_url.as_deref(),
        Some("https://tracker.example/details/88123")
    );
    assert_eq!(
        a.download_url.as_deref(),
        Some("https://tracker.example/dl/88123.torrent?passkey=abc")
    );
    assert_eq!(a.size, Some(2_254_857_830));
    assert_eq!(a.categories, [5000, 5040]);
    assert_eq!((a.seeders, a.peers, a.grabs), (Some(142), Some(160), Some(901)));
    assert_eq!(
        a.info_hash.as_deref(),
        Some("0a1b2c3d4e5f60718293a4b5c6d7e8f901234567")
    );
    assert_eq!(a.imdb_id.as_deref(), Some("tt7654321"));
    assert_eq!(a.tvdb_id, Some(398811));
    assert_eq!(a.minimum_seed_time, Some(172_800));
    assert_eq!(a.minimum_ratio, Some(1.0));
    assert!(
        a.indexer_flags.contains(&IndexerFlag::Freeleech) && a.indexer_flags.contains(&IndexerFlag::Internal)
    );
    assert_eq!(a.publish_date.unwrap().to_rfc3339(), "2026-04-28T10:15:00+00:00");
    let b = &r[1];
    assert!(b.download_url.is_none());
    assert!(
        b.magnet_url
            .as_deref()
            .unwrap()
            .starts_with("magnet:?xt=urn:btih:FFEE")
    );
    assert_eq!(
        b.info_hash.as_deref(),
        Some("ffeeddccbbaa99887766554433221100ffeeddcc")
    );
    assert_eq!((b.size, b.peers), (Some(48_318_382_080), Some(42)));
    assert!(
        b.indexer_flags.contains(&IndexerFlag::Halfleech)
            && b.indexer_flags.contains(&IndexerFlag::DoubleUpload)
    );

    // Caps are cached: the caps mock expects exactly one call.
    idx.capabilities().await.unwrap();
}

#[tokio::test]
async fn newznab_movie_search_rss_and_download() {
    let server = MockServer::start().await;
    caps(&server, "newznab_caps.xml").await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "movie"))
        .and(query_param("imdbid", "0123456"))
        .and(query_param("apikey", "k"))
        .and(query_param("extended", "1"))
        .and(query_param("attrs", "poster"))
        .respond_with(xml(fixture("newznab_search.xml")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api"))
        .and(query_param("t", "search"))
        .and(query_param_is_missing("q"))
        .and(query_param("cat", "2000,5000"))
        .respond_with(xml(fixture("newznab_search.xml")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/getnzb/5f2c9a1e.nzb"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(fixture("sample.nzb"), "application/x-nzb"))
        .mount(&server)
        .await;

    let settings =
        json!({"base_url": server.uri(), "api_key": "k", "additional_parameters": "&attrs=poster"});
    let idx = build("newznab", &settings, &ctx(1, "Usenet")).unwrap();
    assert_eq!(idx.protocol(), Protocol::Usenet);
    let q = SearchQuery {
        imdb_id: Some("tt0123456".into()),
        year: Some(2024),
        ..SearchQuery::movie()
    };
    let r = idx.search(&q).await.unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].categories, [2000, 2040]);
    assert_eq!(r[0].grabs, Some(57));
    assert_eq!(r[0].imdb_id.as_deref(), Some("tt0123456"));
    assert_eq!(
        r[0].publish_date.unwrap().to_rfc3339(),
        "2026-04-29T05:41:12+00:00",
        "usenetdate wins"
    );
    assert_eq!(r[1].size, Some(3_221_225_472), "size from enclosure length");
    assert_eq!(r[1].guid, "9b8a7c6d");
    assert!(r[0].indexer_flags.is_empty());

    assert_eq!(idx.rss(&[]).await.unwrap().len(), 2);
    idx.test().await.unwrap();

    let url = r[0]
        .download_url
        .clone()
        .unwrap()
        .replace("https://usenet.example", &server.uri());
    let nzb = idx.download(&url).await.unwrap();
    assert!(String::from_utf8(nzb).unwrap().contains("<nzb"));
}

#[tokio::test]
async fn newznab_error_codes_are_typed() {
    let server = MockServer::start().await;
    Mock::given(query_param("apikey", "bad"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("newznab_error_apikey.xml")))
        .mount(&server)
        .await;
    Mock::given(query_param("apikey", "limited"))
        .respond_with(ResponseTemplate::new(429).set_body_string(fixture("newznab_error_limit.xml")))
        .mount(&server)
        .await;
    Mock::given(query_param("apikey", "busy"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "120")
                .set_body_string("slow down"),
        )
        .mount(&server)
        .await;
    Mock::given(query_param("apikey", "html"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body>maintenance<br></body></html>"))
        .mount(&server)
        .await;
    let mk = |key: &str| {
        build(
            "newznab",
            &json!({"base_url": server.uri(), "api_key": key}),
            &ctx(1, "n"),
        )
        .unwrap()
    };

    assert!(matches!(mk("bad").test().await, Err(IndexerError::Auth(m)) if m.contains("Incorrect")));
    assert!(matches!(
        mk("limited").test().await,
        Err(IndexerError::RequestLimit { retry_after: None })
    ));
    match mk("busy").test().await {
        Err(IndexerError::RequestLimit { retry_after: Some(d) }) => assert_eq!(d.as_secs(), 120),
        other => panic!("{other:?}"),
    }
    assert!(matches!(mk("html").test().await, Err(IndexerError::Parse(_))));
}

#[tokio::test]
async fn tv_search_falls_back_to_text_when_mode_unavailable() {
    let server = MockServer::start().await;
    let caps_xml = r#"<caps><searching><search available="yes" supportedParams="q"/><tv-search available="no"/></searching></caps>"#;
    Mock::given(query_param("t", "caps"))
        .respond_with(xml(caps_xml.into()))
        .mount(&server)
        .await;
    Mock::given(query_param("t", "search"))
        .and(query_param("q", "Copper Valley S02E05"))
        .respond_with(xml(fixture("torznab_search.xml")))
        .mount(&server)
        .await;
    let idx = build(
        "torznab",
        &json!({"base_url": format!("{}/", server.uri())}),
        &ctx(1, "t"),
    )
    .unwrap();
    let q = SearchQuery {
        term: Some("Copper Valley".into()),
        season: Some(2),
        episode: Some("5".into()),
        ..SearchQuery::tv()
    };
    assert_eq!(idx.search(&q).await.unwrap().len(), 2);
    // An id-only query the indexer can't express yields nothing rather than an unfiltered feed.
    let q = SearchQuery {
        tvdb_id: Some(1),
        ..SearchQuery::tv()
    };
    assert!(idx.search(&q).await.unwrap().is_empty());
}

#[tokio::test]
async fn torznab_download_redirecting_to_magnet() {
    let server = MockServer::start().await;
    Mock::given(path("/dl/1"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "magnet:?xt=urn:btih:abc"))
        .mount(&server)
        .await;
    Mock::given(path("/dl/2"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"d8:announce3:urle".to_vec()))
        .mount(&server)
        .await;
    let idx = build("torznab", &json!({"base_url": server.uri()}), &ctx(1, "t")).unwrap();
    match idx.download(&format!("{}/dl/1", server.uri())).await {
        Err(IndexerError::MagnetRedirect(m)) => assert_eq!(m, "magnet:?xt=urn:btih:abc"),
        other => panic!("{other:?}"),
    }
    assert_eq!(idx.download("/dl/2").await.unwrap(), b"d8:announce3:urle");
}

#[tokio::test]
async fn http_proxy_is_used() {
    // The mock server doubles as an HTTP proxy: requests for an unresolvable host reach it.
    let proxy = MockServer::start().await;
    Mock::given(query_param("t", "caps"))
        .respond_with(xml(fixture("torznab_caps.xml")))
        .expect(1)
        .mount(&proxy)
        .await;
    let addr = proxy.address();
    let mut c = ctx(1, "p");
    c.proxy = Some(ProxyConfig {
        kind: ProxyKind::Http,
        host: addr.ip().to_string(),
        port: addr.port(),
        username: None,
        password: None,
    });
    let idx = build("torznab", &json!({"base_url": "http://indexer.invalid"}), &c).unwrap();
    assert!(idx.capabilities().await.unwrap().movie_search.available);
}

/// with private addresses blocked, a site the local DNS cannot resolve is still searched
/// through the proxy, which looks it up itself.
#[tokio::test]
async fn proxy_resolves_a_site_the_local_dns_cannot() {
    let proxy = MockServer::start().await;
    Mock::given(query_param("t", "caps"))
        .respond_with(xml(fixture("torznab_caps.xml")))
        .expect(1)
        .mount(&proxy)
        .await;
    Mock::given(query_param("q", "Copper Valley"))
        .respond_with(xml(fixture("torznab_search.xml")))
        .expect(1)
        .mount(&proxy)
        .await;
    let addr = proxy.address();
    let mut c = ctx(1, "p");
    c.allow_private_addresses = false;
    c.proxy = Some(ProxyConfig {
        kind: ProxyKind::Http,
        host: addr.ip().to_string(),
        port: addr.port(),
        username: None,
        password: None,
    });
    let idx = build("torznab", &json!({"base_url": "http://indexer.invalid"}), &c).unwrap();
    assert!(idx.capabilities().await.unwrap().movie_search.available);
    let q = SearchQuery {
        term: Some("Copper Valley".into()),
        ..SearchQuery::tv()
    };
    assert_eq!(idx.search(&q).await.unwrap().len(), 2);

    // Without the proxy the local DNS is asked, and fails for the same site.
    c.proxy = None;
    let idx = build("torznab", &json!({"base_url": "http://indexer.invalid"}), &c).unwrap();
    assert!(matches!(
        idx.capabilities().await,
        Err(IndexerError::Blocked(m)) if m.contains("cannot resolve")
    ));
    // Behind the proxy, private literals are still refused.
    c.proxy = Some(ProxyConfig {
        kind: ProxyKind::Http,
        host: addr.ip().to_string(),
        port: addr.port(),
        username: None,
        password: None,
    });
    assert!(matches!(
        build("torznab", &json!({"base_url": "http://192.168.1.10:9117"}), &c),
        Err(IndexerError::Blocked(_))
    ));
}

#[tokio::test]
async fn build_validation_and_ssrf_guard() {
    assert!(matches!(
        build("nope", &json!({}), &ctx(1, "x")),
        Err(IndexerError::UnknownImplementation(_))
    ));
    assert!(
        matches!(build("newznab", &json!({"base_url": "https://u.example"}), &ctx(1, "x")), Err(IndexerError::Config(m)) if m.contains("API key"))
    );
    assert!(matches!(
        build("torznab", &json!({"base_url": "not a url"}), &ctx(1, "x")),
        Err(IndexerError::Config(_))
    ));

    let mut strict = ctx(1, "x");
    strict.allow_private_addresses = false;
    assert!(matches!(
        build(
            "torznab",
            &json!({"base_url": "http://192.168.1.10:9117"}),
            &strict
        ),
        Err(IndexerError::Blocked(_))
    ));
    let idx = build("torznab", &json!({"base_url": "https://93.184.216.34"}), &strict).unwrap();
    assert!(matches!(
        idx.download("http://10.0.0.5/file.torrent").await,
        Err(IndexerError::Blocked(_))
    ));

    let s = schemas();
    assert_eq!(
        s.iter().map(|s| s.implementation.as_str()).collect::<Vec<_>>(),
        ["torznab", "newznab", "slskd"]
    );
    let nz = &s[1];
    assert_eq!(nz.secret_fields().collect::<Vec<_>>(), ["api_key"]);
    assert!(
        nz.fields
            .iter()
            .any(|f| f.name == "categories" && f.kind == FieldKind::List)
    );
}
