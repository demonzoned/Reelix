//! Open Library client (books): authors, works, editions and covers. No API key needed.
//!
//! Endpoints: `/search/authors.json`, `/search.json`, `/authors/{id}.json`,
//! `/authors/{id}/works.json`, `/works/{id}.json`, `/works/{id}/ratings.json`,
//! `/works/{id}/editions.json`, `/isbn/{isbn}.json` and `/trending/weekly.json`; covers come from
//! `covers.openlibrary.org`.

mod wire;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::NaiveDate;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::MetaError;
use crate::books::{
    AuthorMetadata, AuthorSearchHit, BookMetadataProvider, EditionMetadata, SeriesRef, WorkAuthor,
    WorkMetadata, WorkSearchHit,
};
use crate::http::{RateLimiter, RetryPolicy, TtlCache, send};
use crate::music::MetadataStore;

pub const DEFAULT_BASE_URL: &str = "https://openlibrary.org";
pub const DEFAULT_COVERS_URL: &str = "https://covers.openlibrary.org";

/// Provider name used as the [`MetadataStore`] namespace.
const PROVIDER: &str = "openlibrary";
/// Page size of the paged endpoints (author works, work editions).
const PAGE: usize = 100;
/// Safety stop for paging (5 000 entries).
const MAX_PAGES: usize = 50;
const SEARCH_LIMIT: usize = 25;
/// Co-authors resolved by name in [`OpenLibrary::work`].
const MAX_AUTHOR_LOOKUPS: usize = 5;
const SEARCH_FIELDS: &str = "key,title,author_key,author_name,first_publish_year,cover_i,isbn,edition_count";

/// Tunables for [`OpenLibrary`].
#[derive(Debug, Clone)]
pub struct OpenLibraryOptions {
    pub base_url: String,
    pub covers_url: String,
    pub retry: RetryPolicy,
    pub cache_ttl: Duration,
    pub max_requests_per_minute: usize,
}

impl Default for OpenLibraryOptions {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            covers_url: DEFAULT_COVERS_URL.into(),
            retry: RetryPolicy::default(),
            cache_ttl: Duration::from_secs(24 * 3600),
            max_requests_per_minute: 60,
        }
    }
}

/// Open Library client. Cheap to clone; clones share the rate limiter and cache.
#[derive(Debug, Clone)]
pub struct OpenLibrary {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    opts: OpenLibraryOptions,
    user_agent: String,
    cache: TtlCache,
    limiter: RateLimiter,
    store: OnceLock<Arc<dyn MetadataStore>>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner")
            .field("opts", &self.opts)
            .field("cache_entries", &self.cache.len())
            .field("store", &self.store.get().is_some())
            .finish_non_exhaustive()
    }
}

impl OpenLibrary {
    pub fn new(http: reqwest::Client, opts: OpenLibraryOptions) -> Self {
        let limiter = RateLimiter::new(opts.max_requests_per_minute, Duration::from_secs(60));
        Self {
            inner: Arc::new(Inner {
                http,
                user_agent: format!(
                    "Reelix/{} ( https://github.com/demonzoned/Reelix )",
                    reelix_core::VERSION
                ),
                opts,
                cache: TtlCache::new(500),
                limiter,
                store: OnceLock::new(),
            }),
        }
    }

    /// Cover image URL for a cover id; `size` is `S`, `M` or `L`.
    pub fn cover_url(&self, cover_id: i64, size: char) -> String {
        format!(
            "{}/b/id/{cover_id}-{size}.jpg",
            self.inner.opts.covers_url.trim_end_matches('/')
        )
    }

    /// Author photo URL for a photo id; `size` is `S`, `M` or `L`.
    pub fn author_photo_url(&self, photo_id: i64, size: char) -> String {
        format!(
            "{}/a/id/{photo_id}-{size}.jpg",
            self.inner.opts.covers_url.trim_end_matches('/')
        )
    }

    /// Author photo URL by Open Library author id (`OL26320A`).
    pub fn author_olid_photo_url(&self, author_id: &str, size: char) -> String {
        format!(
            "{}/a/olid/{author_id}-{size}.jpg",
            self.inner.opts.covers_url.trim_end_matches('/')
        )
    }

    /// Cover URL by ISBN (Open Library serves a blank image when it has none).
    pub fn isbn_cover_url(&self, isbn: &str, size: char) -> String {
        format!(
            "{}/b/isbn/{}-{size}.jpg",
            self.inner.opts.covers_url.trim_end_matches('/'),
            normalize_isbn(isbn).unwrap_or_default()
        )
    }

    /// Installs a persistent response cache; only the first call has an effect.
    pub fn set_store(&self, store: Arc<dyn MetadataStore>) -> bool {
        self.inner.store.set(store).is_ok()
    }

    /// Drops every in-memory cached response.
    pub fn clear_cache(&self) {
        self.inner.cache.clear();
    }

    fn cover(&self, ids: &[i64]) -> Option<String> {
        ids.iter().find(|id| **id > 0).map(|id| self.cover_url(*id, 'L'))
    }

    /// Works trending on Open Library this week (most read / wanted), up to `limit`.
    pub async fn trending_works(&self, limit: usize) -> Result<Vec<WorkSearchHit>, MetaError> {
        let res: wire::TrendingWorks = self
            .get("/trending/weekly.json", &[("limit", limit.to_string())])
            .await?;
        Ok(res.works.into_iter().filter_map(|d| self.search_hit(d)).collect())
    }

    /// Works with the subject `subject` (an Open Library subject key such as `fantasy` or
    /// `science_fiction`), currently trending first; up to `limit`.
    pub async fn subject_works(&self, subject: &str, limit: usize) -> Result<Vec<WorkSearchHit>, MetaError> {
        let subject = subject.trim();
        if subject.is_empty() || !subject.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(MetaError::NotFound);
        }
        let query = [
            ("q", format!("subject_key:{subject}")),
            ("sort", "trending".to_string()),
            ("fields", SEARCH_FIELDS.to_string()),
            ("limit", limit.to_string()),
        ];
        let res: wire::SearchWorks = self.get("/search.json", &query).await?;
        Ok(res.docs.into_iter().filter_map(|d| self.search_hit(d)).collect())
    }

    /// Maps a search / trending document to a hit (`None` without a work id).
    fn search_hit(&self, d: wire::SearchWorkDoc) -> Option<WorkSearchHit> {
        let id = olid(&d.key, 'W')?;
        let mut isbns: Vec<String> = d.isbn.iter().filter_map(|i| normalize_isbn(i)).collect();
        let mut seen = std::collections::HashSet::new();
        isbns.retain(|i| seen.insert(i.clone()));
        // ISBN-13 first, otherwise in Open Library's order.
        isbns.sort_by_key(|i| std::cmp::Reverse(i.len()));
        isbns.truncate(20);
        Some(WorkSearchHit {
            id,
            title: d.title,
            author_ids: d.author_key.iter().filter_map(|k| olid(k, 'A')).collect(),
            author_names: d.author_name,
            first_publish_year: d.first_publish_year,
            cover_url: d.cover_i.filter(|c| *c > 0).map(|c| self.cover_url(c, 'M')),
            isbns,
            edition_count: d.edition_count,
        })
    }

    async fn get_value(&self, path: &str, query: &[(&str, String)]) -> Result<Arc<Value>, MetaError> {
        let inner = &self.inner;
        let key = format!("{path}?{query:?}");
        if let Some(v) = inner.cache.get(&key) {
            return Ok(v);
        }
        let ttl = inner.opts.cache_ttl;
        if let Some(store) = inner.store.get()
            && let Some(raw) = store.load(PROVIDER, &key).await
            && let Ok(v) = serde_json::from_str::<Value>(&raw)
        {
            let v = Arc::new(v);
            inner.cache.insert(key, v.clone(), ttl);
            return Ok(v);
        }
        let url = format!("{}{}", inner.opts.base_url.trim_end_matches('/'), path);
        let resp = send(
            || {
                inner
                    .http
                    .get(&url)
                    .query(query)
                    .header(reqwest::header::USER_AGENT, &inner.user_agent)
                    .header(reqwest::header::ACCEPT, "application/json")
            },
            Some(&inner.limiter),
            &inner.opts.retry,
        )
        .await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        if !status.is_success() {
            return Err(match status {
                StatusCode::NOT_FOUND => MetaError::NotFound,
                StatusCode::TOO_MANY_REQUESTS => MetaError::RateLimited {
                    retry_after_secs: None,
                },
                _ => MetaError::Status {
                    status: status.as_u16(),
                    message: String::from_utf8_lossy(&body).chars().take(200).collect(),
                },
            });
        }
        let value = serde_json::from_slice::<Value>(&body)?;
        if let Some(store) = inner.store.get() {
            store.save(PROVIDER, &key, &value.to_string(), ttl).await;
        }
        let value = Arc::new(value);
        inner.cache.insert(key, value.clone(), ttl);
        Ok(value)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T, MetaError> {
        let v = self.get_value(path, query).await?;
        Ok(T::deserialize(&*v)?)
    }

    /// Every entry of a paged list endpoint.
    async fn paged<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>, MetaError> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        for _ in 0..MAX_PAGES {
            let page: wire::Paged<T> = self
                .get(
                    path,
                    &[("limit", PAGE.to_string()), ("offset", offset.to_string())],
                )
                .await?;
            let n = page.entries.len();
            out.extend(page.entries);
            offset += n;
            let more = match page.size {
                Some(size) => offset < size as usize,
                None => page.links.next.is_some() || n >= PAGE,
            };
            if n == 0 || !more {
                break;
            }
        }
        Ok(out)
    }

    fn work_from(&self, w: wire::Work, names: &HashMap<String, String>) -> WorkMetadata {
        let authors = w
            .authors
            .iter()
            .filter_map(|a| {
                let id = olid(a.author_key()?, 'A')?;
                Some(WorkAuthor {
                    name: names.get(&id).cloned().unwrap_or_default(),
                    id,
                    role: a.role.clone().filter(|r| !r.trim().is_empty()),
                })
            })
            .collect();
        let first = w.first_publish_date.as_deref().map(str::trim).unwrap_or("");
        WorkMetadata {
            id: olid(&w.key, 'W').unwrap_or_default(),
            title: w.title.trim().to_string(),
            subtitle: nonempty(w.subtitle),
            description: nonempty(w.description.map(wire::Text::into_string)),
            authors,
            first_publish_date: parse_date(first),
            first_publish_year: year_of(first),
            subjects: w.subjects,
            cover_url: self.cover(&w.covers),
            series: w
                .series
                .iter()
                .filter_map(|s| s.as_text().and_then(parse_series))
                .collect(),
            rating: None,
            rating_count: None,
        }
    }

    fn edition_from(&self, e: wire::Edition) -> EditionMetadata {
        let mut narrators: Vec<String> = e
            .contributors
            .iter()
            .filter(|c| is_narrator_role(&c.role))
            .map(|c| c.name.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect();
        for c in &e.contributions {
            if let Some(n) = narrator_of(c)
                && !narrators.contains(&n)
            {
                narrators.push(n);
            }
        }
        let is_audiobook = e.physical_format.as_deref().is_some_and(is_audio_format) || !narrators.is_empty();
        let isbn10 = e.isbn_10.iter().find_map(|i| normalize_isbn(i));
        let asin = e
            .identifiers
            .get("amazon")
            .into_iter()
            .flatten()
            .map(|a| a.trim())
            .find(|a| a.len() == 10 && a.starts_with('B') && a.chars().all(|c| c.is_ascii_alphanumeric()))
            .map(String::from);
        EditionMetadata {
            id: olid(&e.key, 'M').unwrap_or_default(),
            title: e.title.trim().to_string(),
            subtitle: nonempty(e.subtitle),
            isbn13: e.isbn_13.iter().find_map(|i| normalize_isbn(i)),
            isbn10,
            asin,
            publishers: e.publishers,
            publish_date: nonempty(e.publish_date),
            language: e
                .languages
                .first()
                .and_then(|l| l.key.rsplit('/').next())
                .filter(|l| !l.is_empty())
                .map(String::from),
            page_count: e.number_of_pages.filter(|n| *n > 0),
            physical_format: nonempty(e.physical_format),
            is_audiobook,
            cover_url: self.cover(&e.covers),
            series: e
                .series
                .iter()
                .filter_map(|s| s.as_text().and_then(parse_series))
                .collect(),
            narrators,
        }
    }

    async fn ratings(&self, work_id: &str) -> Option<(Option<f64>, Option<u32>)> {
        let r: wire::Ratings = self
            .get(&format!("/works/{work_id}/ratings.json"), &[])
            .await
            .map_err(|e| tracing::debug!(work_id, error = %e, "open library ratings unavailable"))
            .ok()?;
        Some((r.summary.average, r.summary.count.filter(|c| *c > 0)))
    }
}

#[async_trait]
impl BookMetadataProvider for OpenLibrary {
    fn name(&self) -> &'static str {
        PROVIDER
    }

    async fn search_authors(&self, q: &str) -> Result<Vec<AuthorSearchHit>, MetaError> {
        let q = q.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let res: wire::SearchAuthors = self
            .get(
                "/search/authors.json",
                &[("q", q.to_string()), ("limit", SEARCH_LIMIT.to_string())],
            )
            .await?;
        Ok(res
            .docs
            .into_iter()
            .filter_map(|d| {
                let id = olid(&d.key, 'A')?;
                Some(AuthorSearchHit {
                    photo_url: Some(self.author_olid_photo_url(&id, 'M')),
                    id,
                    name: d.name,
                    birth_date: nonempty(d.birth_date),
                    top_work: nonempty(d.top_work),
                    work_count: d.work_count,
                })
            })
            .collect())
    }

    /// Free text (Open Library search syntax such as `author:` works) or a bare ISBN.
    async fn search_works(&self, q: &str) -> Result<Vec<WorkSearchHit>, MetaError> {
        let q = q.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = vec![
            ("fields", SEARCH_FIELDS.to_string()),
            ("limit", SEARCH_LIMIT.to_string()),
        ];
        match normalize_isbn(q) {
            Some(isbn) => query.push(("isbn", isbn)),
            None => query.push(("q", q.to_string())),
        }
        let res: wire::SearchWorks = self.get("/search.json", &query).await?;
        Ok(res.docs.into_iter().filter_map(|d| self.search_hit(d)).collect())
    }

    async fn author(&self, id: &str) -> Result<AuthorMetadata, MetaError> {
        let id = olid(id, 'A').ok_or(MetaError::NotFound)?;
        let a: wire::Author = self.get(&format!("/authors/{id}.json"), &[]).await?;
        let name = if a.name.trim().is_empty() {
            a.personal_name.clone().unwrap_or_default()
        } else {
            a.name.trim().to_string()
        };
        let mut links: Vec<String> = a
            .links
            .iter()
            .map(|l| l.url.trim().to_string())
            .filter(|u| !u.is_empty())
            .collect();
        if let Some(w) = a.wikipedia.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
            links.push(w.to_string());
        }
        if let Some(q) = a.remote_ids.get("wikidata") {
            links.push(format!("https://www.wikidata.org/wiki/{q}"));
        }
        links.dedup();
        let mut alternate_names = a.alternate_names;
        for n in [a.personal_name, a.fuller_name].into_iter().flatten() {
            if n != name && !n.trim().is_empty() && !alternate_names.contains(&n) {
                alternate_names.push(n);
            }
        }
        Ok(AuthorMetadata {
            id: olid(&a.key, 'A').unwrap_or(id),
            sort_name: sort_name(&name),
            name,
            bio: nonempty(a.bio.map(wire::Text::into_string)),
            birth_date: nonempty(a.birth_date),
            death_date: nonempty(a.death_date),
            photo_url: a
                .photos
                .iter()
                .find(|p| **p > 0)
                .map(|p| self.author_photo_url(*p, 'L')),
            links,
            alternate_names,
        })
    }

    /// Co-authors other than `id` are returned with an empty `name` (ids only).
    async fn author_works(&self, id: &str) -> Result<Vec<WorkMetadata>, MetaError> {
        let id = olid(id, 'A').ok_or(MetaError::NotFound)?;
        let works: Vec<wire::Work> = self.paged(&format!("/authors/{id}/works.json")).await?;
        let mut names = HashMap::new();
        match self.author(&id).await {
            Ok(a) => {
                names.insert(id.clone(), a.name);
            }
            Err(e) => tracing::debug!(author = %id, error = %e, "author name unavailable"),
        }
        Ok(works
            .into_iter()
            .map(|w| self.work_from(w, &names))
            .filter(|w| !w.id.is_empty())
            .collect())
    }

    async fn work(&self, id: &str) -> Result<WorkMetadata, MetaError> {
        let id = olid(id, 'W').ok_or(MetaError::NotFound)?;
        let w: wire::Work = self.get(&format!("/works/{id}.json"), &[]).await?;
        let mut names = HashMap::new();
        for key in w
            .authors
            .iter()
            .filter_map(|a| olid(a.author_key()?, 'A'))
            .take(MAX_AUTHOR_LOOKUPS)
        {
            match self.author(&key).await {
                Ok(a) => {
                    names.insert(key, a.name);
                }
                Err(e) => tracing::debug!(author = %key, error = %e, "author name unavailable"),
            }
        }
        let mut work = self.work_from(w, &names);
        if work.id.is_empty() {
            work.id = id.clone();
        }
        if let Some((rating, count)) = self.ratings(&id).await {
            work.rating = rating;
            work.rating_count = count;
        }
        Ok(work)
    }

    async fn work_editions(&self, work_id: &str) -> Result<Vec<EditionMetadata>, MetaError> {
        let id = olid(work_id, 'W').ok_or(MetaError::NotFound)?;
        let editions: Vec<wire::Edition> = self.paged(&format!("/works/{id}/editions.json")).await?;
        Ok(editions
            .into_iter()
            .map(|e| self.edition_from(e))
            .filter(|e| !e.id.is_empty())
            .collect())
    }

    async fn by_isbn(&self, isbn: &str) -> Result<(WorkMetadata, EditionMetadata), MetaError> {
        let isbn = normalize_isbn(isbn).ok_or(MetaError::NotFound)?;
        let e: wire::Edition = self.get(&format!("/isbn/{isbn}.json"), &[]).await?;
        let work_key = e
            .works
            .first()
            .map(|w| w.key.clone())
            .ok_or_else(|| MetaError::Decode(format!("edition for ISBN {isbn} has no work")))?;
        let work = self.work(&work_key).await?;
        let mut edition = self.edition_from(e);
        if edition.isbn13.is_none() && isbn.len() == 13 {
            edition.isbn13 = Some(isbn);
        } else if edition.isbn10.is_none() && isbn.len() == 10 {
            edition.isbn10 = Some(isbn);
        }
        Ok((work, edition))
    }
}

fn nonempty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Normalizes `OL26320A`, `/authors/OL26320A` or a full URL to the bare id; `kind` is the
/// trailing letter (`A` author, `W` work, `M` edition).
pub(crate) fn olid(raw: &str, kind: char) -> Option<String> {
    let last = raw.trim().trim_end_matches('/').rsplit('/').next()?;
    let id = last.strip_suffix(".json").unwrap_or(last).to_ascii_uppercase();
    let digits = id.strip_prefix("OL")?.strip_suffix(kind)?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(id)
}

/// Strips separators from an ISBN-10/13; `None` unless it has the right shape.
pub(crate) fn normalize_isbn(raw: &str) -> Option<String> {
    let s: String = raw
        .trim()
        .trim_start_matches("ISBN")
        .trim_start_matches("isbn")
        .trim_start_matches(':')
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let ok = match s.len() {
        13 => s.bytes().all(|b| b.is_ascii_digit()),
        10 => {
            s[..9].bytes().all(|b| b.is_ascii_digit())
                && (s.as_bytes()[9].is_ascii_digit() || s.as_bytes()[9] == b'X')
        }
        _ => false,
    };
    ok.then_some(s)
}

/// `J.R.R. Tolkien` → `Tolkien, J.R.R.`.
pub(crate) fn sort_name(name: &str) -> String {
    let name = name.trim();
    if name.contains(',') {
        return name.to_string();
    }
    let parts: Vec<&str> = name.split_whitespace().collect();
    let suffixes = ["jr", "jr.", "sr", "sr.", "ii", "iii", "iv"];
    let mut last = parts.len();
    if last > 2 && suffixes.contains(&parts[last - 1].to_ascii_lowercase().as_str()) {
        last -= 1;
    }
    if last < 2 {
        return name.to_string();
    }
    // Surname particles stay with the surname (`Le Guin`, `van Gogh`).
    let particles = [
        "le", "la", "de", "du", "da", "di", "del", "van", "von", "der", "den", "ter", "ten",
    ];
    let mut first = last - 1;
    while first > 1 && particles.contains(&parts[first - 1].to_ascii_lowercase().as_str()) {
        first -= 1;
    }
    let mut s = format!("{}, {}", parts[first..last].join(" "), parts[..first].join(" "));
    for p in &parts[last..] {
        s.push(' ');
        s.push_str(p);
    }
    s
}

/// First four-digit year in a free-form date.
pub(crate) fn year_of(s: &str) -> Option<i32> {
    let b = s.as_bytes();
    (0..b.len().saturating_sub(3)).find_map(|i| {
        let w = &b[i..i + 4];
        let boundary_ok =
            (i == 0 || !b[i - 1].is_ascii_digit()) && b.get(i + 4).is_none_or(|c| !c.is_ascii_digit());
        (boundary_ok && w.iter().all(u8::is_ascii_digit))
            .then(|| s[i..i + 4].parse().ok())
            .flatten()
    })
}

/// Full dates in the formats Open Library uses (`1937-09-21`, `September 21, 1937`, `21 Sep 1937`).
pub(crate) fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim().trim_end_matches('.');
    [
        "%Y-%m-%d",
        "%B %d, %Y",
        "%b %d, %Y",
        "%d %B %Y",
        "%d %b %Y",
        "%B %d %Y",
        "%b %d %Y",
    ]
    .iter()
    .find_map(|f| NaiveDate::parse_from_str(s, f).ok())
}

fn position_of(s: &str) -> Option<String> {
    let mut rest = s.trim();
    let lower = rest.to_ascii_lowercase();
    for p in [
        "volume", "vol.", "vol", "book", "part", "number", "no.", "no", "v.", "#", "bk.",
    ] {
        if lower.starts_with(p) {
            rest = rest[p.len()..].trim_start();
            break;
        }
    }
    let rest = rest.trim_start_matches('#');
    let num: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let num = num.trim_end_matches('.');
    (!num.is_empty() && num.starts_with(|c: char| c.is_ascii_digit())).then(|| num.to_string())
}

/// Parses Open Library's string-encoded series (`The Lord of the Rings ; 1`, `Discworld #3`,
/// `Dune Chronicles, book 2`, `Foundation (2)`).
pub(crate) fn parse_series(raw: &str) -> Option<SeriesRef> {
    let s = raw.trim().trim_end_matches(['.', ',']).trim();
    if s.is_empty() {
        return None;
    }
    let make = |title: &str, position: Option<String>| {
        let title = title.trim().trim_end_matches([',', ';', '-']).trim();
        (!title.is_empty()).then(|| SeriesRef {
            title: title.to_string(),
            position,
        })
    };
    if let Some((title, pos)) = s.rsplit_once(';') {
        return make(title, position_of(pos));
    }
    if let Some(open) = s.rfind('(')
        && s.ends_with(')')
        && let Some(pos) = position_of(&s[open + 1..s.len() - 1])
    {
        return make(&s[..open], Some(pos));
    }
    if let Some((title, pos)) = s.rsplit_once('#')
        && let Some(pos) = position_of(pos)
    {
        return make(title, Some(pos));
    }
    if let Some((title, pos)) = s.rsplit_once(',')
        && let Some(pos) = position_of(pos)
    {
        return make(title, Some(pos));
    }
    make(s, None)
}

fn is_narrator_role(role: &str) -> bool {
    let r = role.trim().to_ascii_lowercase();
    matches!(
        r.as_str(),
        "narrator" | "reader" | "read by" | "narrated by" | "performer"
    )
}

/// `Rob Inglis (Narrator)`, `Read by Rob Inglis`, `Narrated by Rob Inglis`.
fn narrator_of(contribution: &str) -> Option<String> {
    let c = contribution.trim();
    let lower = c.to_ascii_lowercase();
    let name = if let Some(open) = c.rfind('(')
        && c.ends_with(')')
        && is_narrator_role(&c[open + 1..c.len() - 1])
    {
        &c[..open]
    } else if lower.starts_with("read by ") {
        &c[8..]
    } else if lower.starts_with("narrated by ") {
        &c[12..]
    } else {
        return None;
    };
    let name = name.trim().trim_end_matches(',').trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn is_audio_format(format: &str) -> bool {
    let f = format.to_ascii_lowercase();
    f.contains("audio")
        || f.contains("mp3")
        || f.contains("cassette")
        || f.contains("audible")
        || f.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| w == "cd")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_urls() {
        let ol = OpenLibrary::new(reqwest::Client::new(), OpenLibraryOptions::default());
        assert_eq!(
            ol.cover_url(8_231_856, 'L'),
            "https://covers.openlibrary.org/b/id/8231856-L.jpg"
        );
        assert_eq!(
            ol.author_photo_url(6_155_606, 'M'),
            "https://covers.openlibrary.org/a/id/6155606-M.jpg"
        );
        assert_eq!(
            ol.isbn_cover_url("978-0-261-10221-7", 'S'),
            "https://covers.openlibrary.org/b/isbn/9780261102217-S.jpg"
        );
        assert_eq!(ol.name(), "openlibrary");
    }

    #[test]
    fn ids_and_isbns() {
        assert_eq!(olid("/works/OL27482W", 'W').as_deref(), Some("OL27482W"));
        assert_eq!(
            olid("https://openlibrary.org/authors/OL26320A.json", 'A').as_deref(),
            Some("OL26320A")
        );
        assert_eq!(olid("ol7353617m", 'M').as_deref(), Some("OL7353617M"));
        assert_eq!(olid("OL27482W", 'A'), None);
        assert_eq!(olid("../etc", 'A'), None);
        assert_eq!(
            normalize_isbn("978-0-261-10221-7").as_deref(),
            Some("9780261102217")
        );
        assert_eq!(
            normalize_isbn("ISBN 0-261-10221-x").as_deref(),
            Some("026110221X")
        );
        assert_eq!(normalize_isbn("the hobbit"), None);
        assert_eq!(normalize_isbn("12345"), None);
    }

    #[test]
    fn names_and_dates() {
        assert_eq!(sort_name("J.R.R. Tolkien"), "Tolkien, J.R.R.");
        assert_eq!(sort_name("Ursula K. Le Guin"), "Le Guin, Ursula K.");
        assert_eq!(sort_name("Vincent van Gogh"), "van Gogh, Vincent");
        assert_eq!(sort_name("Martin Luther King Jr."), "King, Martin Luther Jr.");
        assert_eq!(sort_name("Homer"), "Homer");
        assert_eq!(sort_name("Tolkien, J.R.R."), "Tolkien, J.R.R.");
        assert_eq!(year_of("September 21, 1937"), Some(1937));
        assert_eq!(year_of("c1954"), Some(1954));
        assert_eq!(year_of("12345"), None);
        assert_eq!(
            parse_date("September 21, 1937"),
            NaiveDate::from_ymd_opt(1937, 9, 21)
        );
        assert_eq!(parse_date("21 Sep 1937"), NaiveDate::from_ymd_opt(1937, 9, 21));
        assert_eq!(parse_date("1937-09-21"), NaiveDate::from_ymd_opt(1937, 9, 21));
        assert_eq!(parse_date("1937"), None);
    }

    #[test]
    fn series_strings() {
        let s = |t: &str, p: Option<&str>| {
            Some(SeriesRef {
                title: t.into(),
                position: p.map(String::from),
            })
        };
        assert_eq!(
            parse_series("The Lord of the Rings ; 1"),
            s("The Lord of the Rings", Some("1"))
        );
        assert_eq!(parse_series("Discworld ; v. 3"), s("Discworld", Some("3")));
        assert_eq!(parse_series("Discworld #3"), s("Discworld", Some("3")));
        assert_eq!(
            parse_series("Dune Chronicles, book 2"),
            s("Dune Chronicles", Some("2"))
        );
        assert_eq!(parse_series("Foundation (2.5)"), s("Foundation", Some("2.5")));
        assert_eq!(
            parse_series("Penguin Modern Classics"),
            s("Penguin Modern Classics", None)
        );
        assert_eq!(parse_series("  "), None);
    }

    #[test]
    fn narrators_and_formats() {
        assert_eq!(
            narrator_of("Rob Inglis (Narrator)").as_deref(),
            Some("Rob Inglis")
        );
        assert_eq!(narrator_of("Read by Andy Serkis").as_deref(), Some("Andy Serkis"));
        assert_eq!(narrator_of("Alan Lee (Illustrator)"), None);
        assert!(is_audio_format("Audio CD"));
        assert!(is_audio_format("MP3 CD"));
        assert!(is_audio_format("cd"));
        assert!(!is_audio_format("Hardcover"));
    }
}
