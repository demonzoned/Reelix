//! Book/music categories: settings, schema fields and `category_for_kind` for every client.

use reelix_core::ContentKind;
use reelix_downloaders::*;
use serde_json::{Value, json};

fn base(implementation: &str, tmp: &std::path::Path) -> Value {
    match implementation {
        "sabnzbd" => json!({ "api_key": "k" }),
        "torrent_blackhole" | "usenet_blackhole" => {
            json!({ "drop_folder": tmp, "watch_folder": tmp })
        }
        _ => json!({}),
    }
}

fn with(mut v: Value, extra: Value) -> Value {
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().cloned().unwrap_or_default());
    v
}

/// (implementation, default movie category, default TV category)
const CATEGORY_CLIENTS: [(&str, &str, &str); 7] = [
    ("sabnzbd", "movies", "tv"),
    ("nzbget", "movies", "tv"),
    ("qbittorrent", "movies-reelix", "tv-reelix"),
    ("transmission", "movies-reelix", "tv-reelix"),
    ("deluge", "movies-reelix", "tv-reelix"),
    ("rtorrent", "movies-reelix", "tv-reelix"),
    ("aria2", "movies-reelix", "tv-reelix"),
];

#[test]
fn schemas_expose_book_and_music_categories() {
    for (imp, _, _) in CATEGORY_CLIENTS {
        let schema = schema(imp).unwrap();
        for name in ["book_category", "music_category"] {
            let f = schema
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("{imp}: no {name}"));
            assert_eq!(f.default, Some(json!("")), "{imp}");
            assert!(f.advanced, "{imp}");
            assert!(
                f.help
                    .as_deref()
                    .unwrap()
                    .contains("Empty = use the movie category"),
                "{imp}"
            );
        }
    }
    for imp in ["torrent_blackhole", "usenet_blackhole"] {
        let schema = schema(imp).unwrap();
        assert!(schema.fields.iter().any(|f| f.name == "book_folder"));
        assert!(schema.fields.iter().any(|f| f.name == "music_folder"));
    }
}

#[test]
fn category_for_kind_defaults_to_the_movie_category() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = BuildContext::default();
    for (imp, movie, tv) in CATEGORY_CLIENTS {
        let c = build(imp, &base(imp, tmp.path()), &ctx).unwrap();
        let k = |kind| c.category_for_kind(kind);
        assert_eq!(k(ContentKind::Movie).as_deref(), Some(movie), "{imp}");
        assert_eq!(k(ContentKind::Series).as_deref(), Some(tv), "{imp}");
        assert_eq!(k(ContentKind::Ebook).as_deref(), Some(movie), "{imp}");
        assert_eq!(k(ContentKind::Audiobook).as_deref(), Some(movie), "{imp}");
        assert_eq!(k(ContentKind::Music).as_deref(), Some(movie), "{imp}");
    }
}

#[test]
fn category_for_kind_uses_the_book_and_music_categories() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = BuildContext::default();
    for (imp, movie, _) in CATEGORY_CLIENTS {
        let settings = with(
            base(imp, tmp.path()),
            json!({ "book_category": " books-reelix ", "music_category": "music-reelix" }),
        );
        let c = build(imp, &settings, &ctx).unwrap();
        let k = |kind| c.category_for_kind(kind);
        assert_eq!(k(ContentKind::Ebook).as_deref(), Some("books-reelix"), "{imp}");
        assert_eq!(
            k(ContentKind::Audiobook).as_deref(),
            Some("books-reelix"),
            "{imp}"
        );
        assert_eq!(k(ContentKind::Music).as_deref(), Some("music-reelix"), "{imp}");
        assert_eq!(k(ContentKind::Movie).as_deref(), Some(movie), "{imp}");
    }
}

#[test]
fn blackhole_category_follows_the_extra_watch_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = BuildContext::default();
    let c = build("usenet_blackhole", &base("usenet_blackhole", tmp.path()), &ctx).unwrap();
    assert_eq!(c.category_for_kind(ContentKind::Ebook), None);
    assert_eq!(c.category_for_kind(ContentKind::Movie), None);
    let books = tmp.path().join("books");
    let s = with(
        base("torrent_blackhole", tmp.path()),
        json!({ "book_folder": books, "music_folder": "" }),
    );
    let c = build("torrent_blackhole", &s, &ctx).unwrap();
    assert_eq!(
        c.category_for_kind(ContentKind::Audiobook).as_deref(),
        Some(blackhole::BOOK_CATEGORY)
    );
    assert_eq!(c.category_for_kind(ContentKind::Music), None);
}
