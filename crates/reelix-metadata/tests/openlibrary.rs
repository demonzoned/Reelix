//! Open Library client against a mocked API with authored fixture payloads.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::NaiveDate;
use reelix_metadata::http::RetryPolicy;
use reelix_metadata::{
    BookMetadataProvider, MetaError, MetadataStore, OpenLibrary, OpenLibraryOptions, SeriesRef,
};
use wiremock::matchers::{header_regex, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> String {
    let p = format!("{}/tests/fixtures/openlibrary/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(p).unwrap()
}

fn ok(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(name), "application/json")
}

fn client(server: &MockServer) -> OpenLibrary {
    OpenLibrary::new(
        reqwest::Client::new(),
        OpenLibraryOptions {
            base_url: server.uri(),
            covers_url: "https://covers.test".into(),
            retry: RetryPolicy {
                max_retries: 2,
                base_backoff: Duration::from_millis(5),
                max_wait: Duration::from_millis(50),
            },
            ..Default::default()
        },
    )
}

async fn mount_author(server: &MockServer) {
    Mock::given(path("/authors/OL26320A.json"))
        .respond_with(ok("author_OL26320A.json"))
        .mount(server)
        .await;
}

async fn mount_work(server: &MockServer) {
    Mock::given(path("/works/OL27482W.json"))
        .respond_with(ok("work_OL27482W.json"))
        .mount(server)
        .await;
    Mock::given(path("/works/OL27482W/ratings.json"))
        .respond_with(ok("ratings_OL27482W.json"))
        .mount(server)
        .await;
}

#[tokio::test]
async fn search_authors_maps_hits_and_sends_user_agent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/authors.json"))
        .and(query_param("q", "tolkien"))
        .and(header_regex("user-agent", r"^Reelix/\S+ \( https://"))
        .respond_with(ok("search_authors_tolkien.json"))
        .expect(1)
        .mount(&server)
        .await;
    let ol = client(&server);
    let hits = ol.search_authors(" tolkien ").await.unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, "OL26320A");
    assert_eq!(hits[0].name, "J.R.R. Tolkien");
    assert_eq!(hits[0].birth_date.as_deref(), Some("3 January 1892"));
    assert_eq!(hits[0].top_work.as_deref(), Some("The Hobbit"));
    assert_eq!(hits[0].work_count, 627);
    assert_eq!(
        hits[0].photo_url.as_deref(),
        Some("https://covers.test/a/olid/OL26320A-M.jpg")
    );
    assert_eq!(hits[1].top_work.as_deref(), Some("The Silmarillion"));
    // Second call is served from the cache (expect(1)); blank queries never hit the API.
    assert_eq!(ol.search_authors("tolkien").await.unwrap(), hits);
    assert!(ol.search_authors("  ").await.unwrap().is_empty());
}

#[tokio::test]
async fn search_works_by_title_and_by_isbn() {
    let server = MockServer::start().await;
    Mock::given(path("/search.json"))
        .and(query_param("q", "the hobbit"))
        .respond_with(ok("search_hobbit.json"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/search.json"))
        .and(query_param("isbn", "9780261102217"))
        .respond_with(ok("search_hobbit.json"))
        .expect(1)
        .mount(&server)
        .await;
    let ol = client(&server);
    let hits = ol.search_works("the hobbit").await.unwrap();
    assert_eq!(hits.len(), 2);
    let h = &hits[0];
    assert_eq!(h.id, "OL27482W");
    assert_eq!(h.title, "The Hobbit");
    assert_eq!(h.author_ids, ["OL26320A"]);
    assert_eq!(h.author_names, ["J.R.R. Tolkien"]);
    assert_eq!(h.first_publish_year, Some(1937));
    assert_eq!(
        h.cover_url.as_deref(),
        Some("https://covers.test/b/id/14627509-M.jpg")
    );
    assert_eq!(h.edition_count, 120);
    assert_eq!(h.isbns, ["9780261102217", "9780618260300", "0261102214"]);
    assert_eq!(hits[1].author_ids, ["OL26320A", "OL2631291A"]);
    assert_eq!(hits[1].cover_url, None);
    // A bare ISBN (with separators) uses the `isbn` field.
    let by_isbn = ol.search_works("978-0-261-10221-7").await.unwrap();
    assert_eq!(by_isbn[0].id, "OL27482W");
}

#[tokio::test]
async fn author_details() {
    let server = MockServer::start().await;
    mount_author(&server).await;
    let ol = client(&server);
    let a = ol.author("/authors/OL26320A").await.unwrap();
    assert_eq!(a.id, "OL26320A");
    assert_eq!(a.name, "J.R.R. Tolkien");
    assert_eq!(a.sort_name, "Tolkien, J.R.R.");
    assert!(
        a.bio
            .as_deref()
            .unwrap()
            .starts_with("John Ronald Reuel Tolkien was")
    );
    assert_eq!(a.birth_date.as_deref(), Some("3 January 1892"));
    assert_eq!(a.death_date.as_deref(), Some("2 September 1973"));
    assert_eq!(
        a.photo_url.as_deref(),
        Some("https://covers.test/a/id/6155606-L.jpg")
    );
    assert_eq!(
        a.links,
        [
            "https://www.tolkienestate.com/",
            "http://en.wikipedia.org/wiki/J._R._R._Tolkien",
            "https://www.wikidata.org/wiki/Q892",
        ]
    );
    assert_eq!(
        a.alternate_names,
        ["J. R. R. Tolkien", "Tolkien", "John Ronald Reuel Tolkien"]
    );
}

#[tokio::test]
async fn author_works_are_paged() {
    let server = MockServer::start().await;
    mount_author(&server).await;
    Mock::given(path("/authors/OL26320A/works.json"))
        .and(query_param("limit", "100"))
        .and(query_param("offset", "0"))
        .respond_with(ok("author_works_page0.json"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/authors/OL26320A/works.json"))
        .and(query_param("offset", "2"))
        .respond_with(ok("author_works_page1.json"))
        .expect(1)
        .mount(&server)
        .await;
    let ol = client(&server);
    let works = ol.author_works("OL26320A").await.unwrap();
    assert_eq!(
        works.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(),
        ["OL27482W", "OL27479W", "OL15331214W"]
    );
    let hobbit = &works[0];
    assert_eq!(hobbit.authors[0].id, "OL26320A");
    assert_eq!(hobbit.authors[0].name, "J.R.R. Tolkien");
    assert_eq!(hobbit.first_publish_date, NaiveDate::from_ymd_opt(1937, 9, 21));
    assert_eq!(hobbit.first_publish_year, Some(1937));
    let towers = &works[1];
    assert_eq!(towers.first_publish_date, None);
    assert_eq!(towers.first_publish_year, Some(1954));
    assert_eq!(
        towers.cover_url.as_deref(),
        Some("https://covers.test/b/id/8231856-L.jpg"),
        "negative cover ids are skipped"
    );
    assert_eq!(
        towers.description.as_deref(),
        Some("The second volume of The Lord of the Rings.")
    );
    assert_eq!(
        towers.series,
        [SeriesRef {
            title: "The Lord of the Rings".into(),
            position: Some("2".into())
        }]
    );
    let annotated = &works[2];
    assert_eq!(annotated.authors.len(), 2);
    assert_eq!(annotated.authors[1].id, "OL2631291A");
    assert_eq!(annotated.authors[1].name, "", "co-authors are ids only");
    assert_eq!(annotated.authors[1].role.as_deref(), Some("Editor"));
    // Newer records link a series by key only: that entry is left out, the work still loads.
    assert_eq!(
        annotated.series,
        [SeriesRef {
            title: "Middle-earth".into(),
            position: None
        }]
    );
}

#[tokio::test]
async fn work_with_author_names_and_ratings() {
    let server = MockServer::start().await;
    mount_author(&server).await;
    mount_work(&server).await;
    let ol = client(&server);
    let w = ol.work("OL27482W").await.unwrap();
    assert_eq!(w.id, "OL27482W");
    assert_eq!(w.title, "The Hobbit");
    assert_eq!(w.subtitle.as_deref(), Some("or There and Back Again"));
    assert!(w.description.as_deref().unwrap().starts_with("Bilbo Baggins"));
    assert_eq!(w.authors.len(), 1);
    assert_eq!(w.authors[0].name, "J.R.R. Tolkien");
    assert_eq!(w.subjects.len(), 4);
    assert_eq!(
        w.cover_url.as_deref(),
        Some("https://covers.test/b/id/14627509-L.jpg")
    );
    assert_eq!(w.rating, Some(4.25));
    assert_eq!(w.rating_count, Some(512));
}

#[tokio::test]
async fn work_without_ratings_still_resolves() {
    let server = MockServer::start().await;
    Mock::given(path("/works/OL27482W.json"))
        .respond_with(ok("work_OL27482W.json"))
        .mount(&server)
        .await;
    // Neither the author nor the ratings endpoint answers: both are best effort.
    let w = client(&server).work("OL27482W").await.unwrap();
    assert_eq!(w.authors[0].id, "OL26320A");
    assert_eq!(w.authors[0].name, "");
    assert_eq!(w.rating, None);
}

#[tokio::test]
async fn work_editions_map_isbn_audiobook_and_series() {
    let server = MockServer::start().await;
    Mock::given(path("/works/OL27482W/editions.json"))
        .and(query_param("limit", "100"))
        .and(query_param("offset", "0"))
        .respond_with(ok("editions_OL27482W.json"))
        .expect(1)
        .mount(&server)
        .await;
    let eds = client(&server).work_editions("OL27482W").await.unwrap();
    assert_eq!(eds.len(), 3);
    let pb = &eds[0];
    assert_eq!(pb.id, "OL7353617M");
    assert_eq!(pb.isbn13.as_deref(), Some("9780261102217"));
    assert_eq!(pb.isbn10.as_deref(), Some("0261102214"));
    assert_eq!(pb.asin, None, "amazon ISBN-10 ids are not ASINs");
    assert_eq!(pb.publishers, ["HarperCollins"]);
    assert_eq!(pb.publish_date.as_deref(), Some("1999"));
    assert_eq!(pb.language.as_deref(), Some("eng"));
    assert_eq!(pb.page_count, Some(310));
    assert_eq!(pb.physical_format.as_deref(), Some("Paperback"));
    assert!(!pb.is_audiobook);
    assert_eq!(
        pb.cover_url.as_deref(),
        Some("https://covers.test/b/id/6979861-L.jpg")
    );

    let audio = &eds[1];
    assert!(audio.is_audiobook);
    assert_eq!(audio.isbn13.as_deref(), Some("9780307969627"));
    assert_eq!(audio.asin.as_deref(), Some("B0099RKRTY"));
    assert_eq!(audio.narrators, ["Rob Inglis"]);
    assert_eq!(
        audio.series,
        [SeriesRef {
            title: "Middle-earth Universe".into(),
            position: Some("1".into())
        }]
    );
    assert_eq!(audio.cover_url, None);

    let de = &eds[2];
    assert!(de.is_audiobook);
    assert_eq!(de.language.as_deref(), Some("ger"));
    assert_eq!(de.page_count, None);
    assert_eq!(de.narrators, ["Gert Heidenreich"]);
}

#[tokio::test]
async fn by_isbn_follows_the_work() {
    let server = MockServer::start().await;
    mount_author(&server).await;
    mount_work(&server).await;
    Mock::given(path("/isbn/9780261102217.json"))
        .respond_with(ok("isbn_9780261102217.json"))
        .expect(1)
        .mount(&server)
        .await;
    let ol = client(&server);
    let (work, edition) = ol.by_isbn("978-0261102217").await.unwrap();
    assert_eq!(work.id, "OL27482W");
    assert_eq!(work.rating, Some(4.25));
    assert_eq!(edition.id, "OL7353617M");
    assert_eq!(edition.isbn13.as_deref(), Some("9780261102217"));
    assert_eq!(edition.isbn10.as_deref(), Some("0261102214"));
    assert_eq!(
        edition.series,
        [SeriesRef {
            title: "Tolkien's Middle-earth".into(),
            position: None
        }]
    );
    assert!(matches!(
        ol.by_isbn("not-an-isbn").await,
        Err(MetaError::NotFound)
    ));
}

#[tokio::test]
async fn errors_and_invalid_ids() {
    let server = MockServer::start().await;
    Mock::given(path("/works/OL1W.json"))
        .respond_with(
            ResponseTemplate::new(404).set_body_string(r#"{"error": "notfound", "key": "/works/OL1W"}"#),
        )
        .mount(&server)
        .await;
    Mock::given(path("/isbn/0000000000.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(path("/search/authors.json"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad query"))
        .mount(&server)
        .await;
    let ol = client(&server);
    assert!(matches!(ol.work("OL1W").await, Err(MetaError::NotFound)));
    assert!(matches!(ol.by_isbn("0000000000").await, Err(MetaError::NotFound)));
    assert!(matches!(
        ol.search_authors("x").await,
        Err(MetaError::Status { status: 400, .. })
    ));
    // Malformed ids never reach the network.
    assert!(matches!(ol.work("../admin").await, Err(MetaError::NotFound)));
    assert!(matches!(ol.author("OL27482W").await, Err(MetaError::NotFound)));
}

#[tokio::test]
async fn server_errors_are_retried() {
    let server = MockServer::start().await;
    Mock::given(path("/authors/OL26320A.json"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    mount_author(&server).await;
    let a = client(&server).author("OL26320A").await.unwrap();
    assert_eq!(a.name, "J.R.R. Tolkien");
}

#[derive(Default)]
struct MemStore {
    rows: Mutex<HashMap<(String, String), String>>,
}

#[async_trait]
impl MetadataStore for MemStore {
    async fn load(&self, provider: &str, key: &str) -> Option<String> {
        self.rows
            .lock()
            .unwrap()
            .get(&(provider.to_string(), key.to_string()))
            .cloned()
    }

    async fn save(&self, provider: &str, key: &str, payload: &str, ttl: Duration) {
        assert_eq!(ttl, Duration::from_secs(24 * 3600));
        self.rows
            .lock()
            .unwrap()
            .insert((provider.to_string(), key.to_string()), payload.to_string());
    }
}

#[tokio::test]
async fn persistent_store_is_written_and_reused() {
    let server = MockServer::start().await;
    Mock::given(path("/authors/OL26320A.json"))
        .respond_with(ok("author_OL26320A.json"))
        .expect(1)
        .mount(&server)
        .await;
    let store = Arc::new(MemStore::default());
    let first = client(&server);
    assert!(first.set_store(store.clone()));
    assert!(!first.set_store(store.clone()), "store is set once");
    let a = first.author("OL26320A").await.unwrap();
    assert_eq!(store.rows.lock().unwrap().len(), 1);
    assert!(store.rows.lock().unwrap().keys().all(|(p, _)| p == "openlibrary"));
    // A fresh client (empty memory cache) is served from the store, not the API (expect(1)).
    let second = client(&server);
    second.set_store(store);
    assert_eq!(second.author("OL26320A").await.unwrap(), a);
}

#[tokio::test]
async fn trending_works_maps_docs_and_skips_non_works() {
    let server = MockServer::start().await;
    Mock::given(path("/trending/weekly.json"))
        .and(query_param("limit", "40"))
        .respond_with(ok("trending_weekly.json"))
        .expect(1)
        .mount(&server)
        .await;
    let ol = client(&server);
    let works = ol.trending_works(40).await.unwrap();
    assert_eq!(works.len(), 2, "the /books/ edition is skipped");
    assert_eq!(works[0].id, "OL17930368W");
    assert_eq!(works[0].title, "Atomic Habits");
    assert_eq!(works[0].author_names, vec!["James Clear"]);
    assert_eq!(works[0].author_ids, vec!["OL7422948A"]);
    assert_eq!(works[0].first_publish_year, Some(2016));
    assert_eq!(
        works[0].cover_url.as_deref(),
        Some("https://covers.test/b/id/12539702-M.jpg")
    );
    assert_eq!(works[1].cover_url, None);
    // Cached: the second call doesn't hit the server (expect(1) above).
    assert_eq!(ol.trending_works(40).await.unwrap(), works);
}

#[tokio::test]
async fn subject_works_searches_the_subject_by_trending() {
    let server = MockServer::start().await;
    Mock::given(path("/search.json"))
        .and(query_param("q", "subject_key:fantasy"))
        .and(query_param("sort", "trending"))
        .and(query_param("limit", "40"))
        .respond_with(ok("subject_fantasy.json"))
        .mount(&server)
        .await;
    let ol = client(&server);
    let works = ol.subject_works("fantasy", 40).await.unwrap();
    let titles: Vec<_> = works.iter().map(|w| w.title.as_str()).collect();
    assert_eq!(titles, ["The Mark of Athena", "The Two Towers"]);
    assert_eq!(works[1].id, "OL27479W");
    assert_eq!(works[1].author_names, vec!["J.R.R. Tolkien"]);
}

#[tokio::test]
async fn subject_works_rejects_subjects_that_would_change_the_query() {
    let server = MockServer::start().await;
    let ol = client(&server);
    for bad in ["", "fantasy OR author:x", "a:b", "sci-fi"] {
        assert!(
            matches!(ol.subject_works(bad, 10).await, Err(MetaError::NotFound)),
            "{bad:?} should be rejected"
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}
