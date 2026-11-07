//! Open Library JSON wire types (private to the client).

use std::collections::HashMap;

use serde::Deserialize;

/// `"text"` or `{"type": "/type/text", "value": "text"}`.
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub(super) enum Text {
    Plain(String),
    Typed { value: String },
}

impl Text {
    pub(super) fn into_string(self) -> String {
        match self {
            Self::Plain(s) | Self::Typed { value: s } => s,
        }
    }
}

/// A series entry: `"Name ; 3"`, or (newer records) `{"series": {"key": "/series/OL…L"}, "position": "3"}`,
/// which names the series only by its key.
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub(super) enum SeriesEntry {
    Plain(String),
    Linked {
        #[allow(dead_code)]
        series: Key,
        #[allow(dead_code)]
        #[serde(default)]
        position: Option<serde_json::Value>,
    },
    /// Anything else (`null`, a number, an object of another shape): ignored, so one odd entry
    /// does not make the whole work or edition unreadable.
    Other(serde::de::IgnoredAny),
}

impl SeriesEntry {
    /// The text form; linked entries carry no name, so they are left out for now.
    pub(super) fn as_text(&self) -> Option<&str> {
        match self {
            Self::Plain(s) => Some(s),
            Self::Linked { .. } | Self::Other(_) => None,
        }
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub(super) struct Key {
    pub key: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct SearchAuthors {
    pub docs: Vec<SearchAuthorDoc>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct SearchAuthorDoc {
    pub key: String,
    pub name: String,
    pub birth_date: Option<String>,
    pub top_work: Option<String>,
    pub work_count: u32,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct SearchWorks {
    pub docs: Vec<SearchWorkDoc>,
}

/// `/trending/{period}.json`: the same documents as search, under `works`.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct TrendingWorks {
    pub works: Vec<SearchWorkDoc>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct SearchWorkDoc {
    pub key: String,
    pub title: String,
    pub author_key: Vec<String>,
    pub author_name: Vec<String>,
    pub first_publish_year: Option<i32>,
    pub cover_i: Option<i64>,
    pub isbn: Vec<String>,
    pub edition_count: u32,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Link {
    pub url: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Author {
    pub key: String,
    pub name: String,
    pub personal_name: Option<String>,
    pub fuller_name: Option<String>,
    pub alternate_names: Vec<String>,
    pub bio: Option<Text>,
    pub birth_date: Option<String>,
    pub death_date: Option<String>,
    pub photos: Vec<i64>,
    pub links: Vec<Link>,
    pub wikipedia: Option<String>,
    pub remote_ids: HashMap<String, String>,
}

/// `{"author": {"key": …}, "type": …}` (current) or `{"key": …}` (legacy).
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct WorkAuthorRole {
    pub author: Option<Key>,
    pub key: Option<String>,
    pub role: Option<String>,
}

impl WorkAuthorRole {
    pub(super) fn author_key(&self) -> Option<&str> {
        self.author
            .as_ref()
            .map(|a| a.key.as_str())
            .or(self.key.as_deref())
            .filter(|k| !k.is_empty())
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Work {
    pub key: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub description: Option<Text>,
    pub authors: Vec<WorkAuthorRole>,
    pub first_publish_date: Option<String>,
    pub subjects: Vec<String>,
    pub covers: Vec<i64>,
    pub series: Vec<SeriesEntry>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Contributor {
    pub role: String,
    pub name: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Edition {
    pub key: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub isbn_13: Vec<String>,
    pub isbn_10: Vec<String>,
    pub identifiers: HashMap<String, Vec<String>>,
    pub publishers: Vec<String>,
    pub publish_date: Option<String>,
    pub languages: Vec<Key>,
    pub number_of_pages: Option<u32>,
    pub physical_format: Option<String>,
    pub covers: Vec<i64>,
    pub series: Vec<SeriesEntry>,
    pub works: Vec<Key>,
    pub contributors: Vec<Contributor>,
    pub contributions: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Links {
    pub next: Option<String>,
}

/// `/authors/{id}/works.json` and `/works/{id}/editions.json`.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub(super) struct Paged<T> {
    pub links: Links,
    pub size: Option<u32>,
    pub entries: Vec<T>,
}

impl<T> Default for Paged<T> {
    fn default() -> Self {
        Self {
            links: Links::default(),
            size: None,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct Ratings {
    pub summary: RatingSummary,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub(super) struct RatingSummary {
    pub average: Option<f64>,
    pub count: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_series_entries_do_not_spoil_the_work() {
        let work: Work = serde_json::from_value(serde_json::json!({
            "key": "/works/OL1W",
            "title": "A book",
            "series": [null, 3, {"name": "no key"}, "Discworld ; 1", {"series": {"key": "/series/OL1L"}}]
        }))
        .unwrap();
        assert_eq!(work.series.len(), 5);
        let texts: Vec<_> = work.series.iter().filter_map(SeriesEntry::as_text).collect();
        assert_eq!(texts, ["Discworld ; 1"]);
        let edition: Edition = serde_json::from_value(serde_json::json!({
            "key": "/books/OL1M",
            "series": [null]
        }))
        .unwrap();
        assert_eq!(edition.series.len(), 1);
    }
}
