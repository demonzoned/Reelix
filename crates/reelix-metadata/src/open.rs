//! Open data: film and series metadata without an API key, for servers with no TMDB key.
//!
//! - **Films** come from [Wikidata](https://www.wikidata.org) (CC0), found by their TMDB id
//!   (property P4947) or IMDb id (P345): title, release date, running time, genres and ids. The
//!   overview is the first paragraph of the film's Wikipedia article (CC BY-SA 4.0).
//! - **Series** come from [TVmaze](https://www.tvmaze.com) (CC BY-SA 4.0), found by their TVDB or
//!   IMDb id: every season and episode with air dates, summaries and the show's poster. A series
//!   known only by its TMDB id is first matched to its TVDB or IMDb id on Wikidata (P4983).
//!
//! TMDB is still preferred when a key is set; this source keeps a library working without one.
//! Not covered: collections, people, film posters (rarely free to use), certifications and
//! discover feeds. TVmaze allows 20 requests every 10 seconds, so its calls are rate limited;
//! everything is cached.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use reelix_core::MediaKind;
use serde_json::Value;

use crate::MetaError;
use crate::http::{RateLimiter, RetryPolicy, TtlCache, send};
use crate::model::*;

pub const DEFAULT_SPARQL_URL: &str = "https://query.wikidata.org/sparql";
pub const DEFAULT_WIKIDATA_API_URL: &str = "https://www.wikidata.org/w/api.php";
/// `{lang}` is replaced with the language's two-letter code.
pub const DEFAULT_WIKIPEDIA_URL: &str = "https://{lang}.wikipedia.org";
pub const DEFAULT_TVMAZE_URL: &str = "https://api.tvmaze.com";

/// Tunables for [`OpenData`].
#[derive(Debug, Clone)]
pub struct OpenDataOptions {
    pub sparql_url: String,
    pub wikidata_api_url: String,
    pub wikipedia_url: String,
    pub tvmaze_url: String,
    /// BCP-47-ish language such as `en-US`; labels and articles use its first part.
    pub language: String,
    pub retry: RetryPolicy,
    pub cache_ttl: Duration,
}

impl Default for OpenDataOptions {
    fn default() -> Self {
        Self {
            sparql_url: DEFAULT_SPARQL_URL.into(),
            wikidata_api_url: DEFAULT_WIKIDATA_API_URL.into(),
            wikipedia_url: DEFAULT_WIKIPEDIA_URL.into(),
            tvmaze_url: DEFAULT_TVMAZE_URL.into(),
            language: "en-US".into(),
            retry: RetryPolicy::default(),
            cache_ttl: Duration::from_secs(24 * 3600),
        }
    }
}

/// Wikidata and TVmaze client. Cheap to clone.
#[derive(Debug, Clone)]
pub struct OpenData {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    opts: OpenDataOptions,
    cache: TtlCache,
    tvmaze: RateLimiter,
    wikidata: RateLimiter,
}

/// One SPARQL result row: variable name to its value.
type Row = HashMap<String, String>;

impl OpenData {
    pub fn new(http: reqwest::Client, opts: OpenDataOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                http,
                opts,
                // At most 32 MiB of answers: the lightweight goal matters more than a few re-fetches.
                cache: TtlCache::new(2048).with_max_bytes(32 * 1024 * 1024),
                // TVmaze: 20 calls per 10 seconds per address. Wikidata asks for few parallel queries.
                tvmaze: RateLimiter::new(18, Duration::from_secs(10)),
                wikidata: RateLimiter::new(5, Duration::from_secs(1)),
            }),
        }
    }

    fn lang(&self) -> String {
        let l = self.inner.opts.language.split(['-', '_']).next().unwrap_or("en");
        if l.len() == 2 && l.chars().all(|c| c.is_ascii_lowercase()) {
            l.into()
        } else {
            "en".into()
        }
    }

    /// GET a JSON document; 404 is `NotFound`. Cached.
    async fn get(
        &self,
        url: &str,
        query: &[(&str, String)],
        limiter: &RateLimiter,
    ) -> Result<Arc<Value>, MetaError> {
        let key = format!("{url}?{query:?}");
        if let Some(v) = self.inner.cache.get(&key) {
            return Ok(v);
        }
        let resp = send(
            || {
                self.inner
                    .http
                    .get(url)
                    .query(query)
                    .header(reqwest::header::ACCEPT, "application/json")
            },
            Some(limiter),
            &self.inner.opts.retry,
        )
        .await?;
        let st = resp.status();
        if st == reqwest::StatusCode::NOT_FOUND {
            return Err(MetaError::NotFound);
        }
        if !st.is_success() {
            return Err(MetaError::Status {
                status: st.as_u16(),
                message: error_text(resp).await,
            });
        }
        let body = body_up_to(resp, MAX_BODY)
            .await?
            .ok_or_else(|| MetaError::Decode(format!("the answer is larger than {} MiB", MAX_BODY >> 20)))?;
        let v: Value = serde_json::from_slice(&body)?;
        let v = Arc::new(v);
        self.inner.cache.insert(key, v.clone(), self.inner.opts.cache_ttl);
        Ok(v)
    }

    async fn sparql(&self, query: String) -> Result<Vec<Row>, MetaError> {
        let url = self.inner.opts.sparql_url.clone();
        let v = self
            .get(
                &url,
                &[("format", "json".into()), ("query", query)],
                &self.inner.wikidata,
            )
            .await?;
        let rows = v
            .pointer("/results/bindings")
            .and_then(Value::as_array)
            .map(|b| {
                b.iter()
                    .filter_map(Value::as_object)
                    .map(|o| {
                        o.iter()
                            .filter_map(|(k, v)| Some((k.clone(), v.get("value")?.as_str()?.to_string())))
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(rows)
    }

    // ------------------------------------------------------------------
    // Films (Wikidata + Wikipedia)
    // ------------------------------------------------------------------

    /// The SPARQL for films matching `filter` (a triple pattern on `?item`). The name is the label in
    /// the language, else the multilingual (`mul`) label Wikidata now uses for many films, else English.
    fn film_query(&self, filter: &str, limit: u32) -> String {
        let lang = self.lang();
        format!(
            r#"SELECT ?item ?label ?tmdb ?imdb ?date ?duration ?article
              (GROUP_CONCAT(DISTINCT ?genreLabel; separator="|") AS ?genres) WHERE {{
              {filter}
              ?item wdt:P4947 ?tmdb.
              OPTIONAL {{ ?item rdfs:label ?labelLang FILTER(lang(?labelLang) = "{lang}") }}
              OPTIONAL {{ ?item rdfs:label ?labelMul FILTER(lang(?labelMul) = "mul") }}
              OPTIONAL {{ ?item rdfs:label ?labelEn FILTER(lang(?labelEn) = "en") }}
              BIND(COALESCE(?labelLang, ?labelMul, ?labelEn) AS ?label)
              OPTIONAL {{ ?item wdt:P345 ?imdb }}
              OPTIONAL {{ ?item wdt:P577 ?date }}
              OPTIONAL {{ ?item wdt:P2047 ?duration }}
              OPTIONAL {{ ?item wdt:P136 ?genre. ?genre rdfs:label ?genreLabel
                          FILTER(lang(?genreLabel) = "{lang}" || (lang(?genreLabel) = "en" && "{lang}" = "en")) }}
              OPTIONAL {{ ?article schema:about ?item; schema:isPartOf <https://{lang}.wikipedia.org/> }}
            }} GROUP BY ?item ?label ?tmdb ?imdb ?date ?duration ?article LIMIT {limit}"#
        )
    }

    /// One row per film: the earliest release date wins (P577 often lists several countries).
    fn films(rows: Vec<Row>) -> Vec<Row> {
        let mut by: Vec<Row> = Vec::new();
        for r in rows {
            match by.iter_mut().find(|x| x.get("item") == r.get("item")) {
                Some(have) => {
                    if r.get("date") < have.get("date") && r.contains_key("date") {
                        have.insert("date".into(), r["date"].clone());
                    }
                    for (k, v) in r {
                        have.entry(k).or_insert(v);
                    }
                }
                None => by.push(r),
            }
        }
        by
    }

    async fn summary(&self, article: Option<&String>) -> Option<String> {
        let title = wiki_title(article?)?;
        let base = self.inner.opts.wikipedia_url.replace("{lang}", &self.lang());
        let url = format!("{}/api/rest_v1/page/summary/{title}", base.trim_end_matches('/'));
        let v = self.get(&url, &[], &self.inner.wikidata).await.ok()?;
        nonempty(v.get("extract").and_then(Value::as_str).map(String::from))
    }

    fn film_hit(r: &Row) -> Option<SearchHit> {
        let date = r.get("date").and_then(|d| parse_date(d));
        Some(SearchHit {
            kind: MediaKind::Movie,
            tmdb_id: r.get("tmdb")?.parse().ok(),
            tvdb_id: None,
            imdb_id: r.get("imdb").cloned(),
            title: r.get("label").cloned().unwrap_or_default(),
            original_title: None,
            year: date.map(|d| chrono::Datelike::year(&d)),
            release_date: date,
            overview: None,
            poster: None,
            backdrop: None,
            genre_ids: Vec::new(),
            original_language: None,
            vote_average: 0.0,
            vote_count: 0,
            popularity: 0.0,
            adult: false,
        })
    }

    /// A film by its TMDB id. A film open data does not have is `Unsupported`, not `NotFound`:
    /// refreshing treats `NotFound` as "removed from TMDB" and would mark the film as deleted.
    pub async fn movie(&self, tmdb_id: TmdbId) -> Result<MovieMetadata, MetaError> {
        self.movie_lookup(tmdb_id).await.map_err(|e| {
            unlisted(
                e,
                "this film is not on Wikidata; add a TMDB key in Settings → Metadata",
            )
        })
    }

    async fn movie_lookup(&self, tmdb_id: TmdbId) -> Result<MovieMetadata, MetaError> {
        let rows = self
            .sparql(self.film_query(&format!(r#"?item wdt:P4947 "{tmdb_id}"."#), 50))
            .await?;
        let r = Self::films(rows).into_iter().next().ok_or(MetaError::NotFound)?;
        let title = r.get("label").cloned().ok_or(MetaError::NotFound)?;
        let date = r.get("date").and_then(|d| parse_date(d));
        let overview = self.summary(r.get("article")).await;
        let today = chrono::Utc::now().date_naive();
        Ok(MovieMetadata {
            tmdb_id,
            imdb_id: r.get("imdb").cloned(),
            sort_title: sort_title(&title),
            original_title: title.clone(),
            title,
            original_language: None,
            overview,
            tagline: None,
            year: date.map(|d| chrono::Datelike::year(&d)),
            release_date: date,
            release_dates: ReleaseDates {
                cinema: date,
                ..Default::default()
            },
            certification: None,
            status: match date {
                Some(d) if d <= today => MovieStatus::Released,
                Some(_) => MovieStatus::Planned,
                None => MovieStatus::Unknown,
            },
            runtime_minutes: r
                .get("duration")
                .and_then(|d| d.parse::<f64>().ok())
                .map(f64::round)
                .filter(|m| (0.0..=f64::from(u32::MAX)).contains(m))
                .map(|m| m as u32),
            genres: genres(r.get("genres")),
            keywords: Vec::new(),
            studios: Vec::new(),
            collection: None,
            alternative_titles: Vec::new(),
            external_ids: ExternalIds {
                tmdb: Some(tmdb_id),
                imdb: r.get("imdb").cloned(),
                tvdb: None,
                wikidata: r.get("item").and_then(|i| i.rsplit('/').next()).map(String::from),
            },
            credits: Credits::default(),
            images: Images::default(),
            videos: Vec::new(),
            ratings: Ratings::default(),
            adult: false,
            homepage: None,
            recommendations: Vec::new(),
            similar: Vec::new(),
        })
    }

    /// Films whose title matches `query` (Wikidata's own search, then only films with a TMDB id).
    pub async fn search_movies(&self, query: &str, year: Option<i32>) -> Result<Vec<SearchHit>, MetaError> {
        let url = self.inner.opts.wikidata_api_url.clone();
        let found = self
            .get(
                &url,
                &[
                    ("action", "wbsearchentities".into()),
                    ("search", query.into()),
                    ("language", self.lang()),
                    ("type", "item".into()),
                    ("limit", "30".into()),
                    ("format", "json".into()),
                ],
                &self.inner.wikidata,
            )
            .await?;
        let ids: Vec<String> = found
            .get("search")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("id")?.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let ids: Vec<&String> = ids.iter().filter(|i| is_qid(i)).collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let values = ids
            .iter()
            .map(|i| format!("wd:{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let rows = self
            .sparql(self.film_query(&format!("VALUES ?item {{ {values} }}"), 200))
            .await?;
        let mut films = Self::films(rows);
        // Keep Wikidata's own order (best match first).
        films.sort_by_key(|r| {
            let q = r
                .get("item")
                .and_then(|i| i.rsplit('/').next())
                .unwrap_or_default();
            ids.iter().position(|i| i.as_str() == q).unwrap_or(usize::MAX)
        });
        let mut hits: Vec<SearchHit> = films.iter().filter_map(Self::film_hit).collect();
        if let Some(y) = year {
            hits.retain(|h| h.year.is_none_or(|hy| (hy - y).abs() <= 1));
        }
        Ok(hits)
    }

    /// A film by its IMDb id (`tt…`).
    pub async fn find_movie_by_imdb(&self, imdb: &str) -> Result<Vec<SearchHit>, MetaError> {
        if !is_imdb(imdb) {
            return Ok(Vec::new());
        }
        let rows = self
            .sparql(self.film_query(&format!(r#"?item wdt:P345 "{imdb}"."#), 10))
            .await?;
        Ok(Self::films(rows).iter().filter_map(Self::film_hit).collect())
    }

    // ------------------------------------------------------------------
    // Series (TVmaze, with Wikidata for TMDB ids)
    // ------------------------------------------------------------------

    fn tvmaze(&self, path: &str) -> String {
        format!("{}{}", self.inner.opts.tvmaze_url.trim_end_matches('/'), path)
    }

    /// TVDB and IMDb ids of a series known by its TMDB id (Wikidata P4983), and the other way round.
    async fn series_ids(&self, filter: &str) -> Result<Option<Row>, MetaError> {
        let q = format!(
            "SELECT ?tmdb ?tvdb ?imdb WHERE {{ {filter} OPTIONAL {{ ?item wdt:P4983 ?tmdb }} \
             OPTIONAL {{ ?item wdt:P4835 ?tvdb }} OPTIONAL {{ ?item wdt:P345 ?imdb }} }} LIMIT 1"
        );
        Ok(self.sparql(q).await?.into_iter().next())
    }

    /// The TVmaze show for an id, or `NotFound`.
    async fn show(&self, id: &SeriesExternalId) -> Result<(Arc<Value>, Option<TmdbId>), MetaError> {
        let (by, value, mut tmdb) = match id {
            SeriesExternalId::Tvdb(t) => ("thetvdb", t.to_string(), None),
            SeriesExternalId::Imdb(i) => ("imdb", i.clone(), None),
            SeriesExternalId::Tmdb(t) => {
                let ids = self
                    .series_ids(&format!(r#"?item wdt:P4983 "{t}"."#))
                    .await?
                    .ok_or(MetaError::NotFound)?;
                match (ids.get("tvdb"), ids.get("imdb")) {
                    (Some(v), _) => ("thetvdb", v.clone(), Some(*t)),
                    (None, Some(v)) => ("imdb", v.clone(), Some(*t)),
                    _ => return Err(MetaError::NotFound),
                }
            }
        };
        let url = self.tvmaze("/lookup/shows");
        let show = match self.get(&url, &[(by, value)], &self.inner.tvmaze).await {
            // TVmaze does not know every show by its TVDB id: try its IMDb id from Wikidata.
            Err(MetaError::NotFound) if by == "thetvdb" => {
                let SeriesExternalId::Tvdb(t) = id else {
                    return Err(MetaError::NotFound);
                };
                let imdb = self
                    .series_ids(&format!(r#"?item wdt:P4835 "{t}"."#))
                    .await?
                    .and_then(|r| r.get("imdb").cloned())
                    .filter(|i| is_imdb(i))
                    .ok_or(MetaError::NotFound)?;
                self.get(&url, &[("imdb", imdb)], &self.inner.tvmaze).await?
            }
            other => other?,
        };
        if tmdb.is_none() {
            // The TMDB id, when Wikidata knows it, so the series matches one added with TMDB later.
            let filter = match id {
                SeriesExternalId::Tvdb(t) => format!(r#"?item wdt:P4835 "{t}"."#),
                SeriesExternalId::Imdb(i) if is_imdb(i) => format!(r#"?item wdt:P345 "{i}"."#),
                _ => String::new(),
            };
            if !filter.is_empty()
                && let Ok(Some(r)) = self.series_ids(&filter).await
            {
                tmdb = r.get("tmdb").and_then(|t| t.parse().ok());
            }
        }
        Ok((show, tmdb))
    }

    /// A series with every season and episode. Specials (TVmaze episodes with no number) become
    /// season 0, numbered in air date order.
    pub async fn series(&self, id: SeriesExternalId) -> Result<SeriesMetadata, MetaError> {
        self.series_lookup(id).await.map_err(|e| {
            unlisted(e, "this series is not on TVmaze, or has no TMDB link on Wikidata; add a TMDB key in Settings → Metadata")
        })
    }

    async fn series_lookup(&self, id: SeriesExternalId) -> Result<SeriesMetadata, MetaError> {
        let (show, tmdb_id) = self.show(&id).await?;
        let show_id = show
            .get("id")
            .and_then(Value::as_u64)
            .ok_or(MetaError::NotFound)?;
        let url = self.tvmaze(&format!("/shows/{show_id}/episodes"));
        let eps = self
            .get(&url, &[("specials", "1".into())], &self.inner.tvmaze)
            .await?;
        let mut episodes: Vec<EpisodeMetadata> = Vec::new();
        let mut specials = 0;
        for e in eps.as_array().into_iter().flatten() {
            let (season, number) = match e.get("number").and_then(Value::as_u64) {
                Some(n) => {
                    let season = e.get("season").and_then(Value::as_u64).unwrap_or(0);
                    // A number past u32 is not a real episode; it is left out rather than cut short.
                    match (u32::try_from(season), u32::try_from(n)) {
                        (Ok(s), Ok(n)) => (s, n),
                        _ => continue,
                    }
                }
                None => {
                    specials += 1;
                    (0, specials)
                }
            };
            episodes.push(EpisodeMetadata {
                season_number: season,
                episode_number: number,
                absolute_number: None,
                title: str_of(e, "name"),
                overview: str_of(e, "summary")
                    .map(|s| plain(&s))
                    .and_then(|s| nonempty(Some(s))),
                air_date: str_of(e, "airdate").and_then(|d| parse_date(&d)),
                runtime_minutes: u32_of(e.get("runtime")),
                still: e
                    .pointer("/image/original")
                    .and_then(Value::as_str)
                    .map(String::from),
                tmdb_id: None,
                tvdb_id: None,
                finale: None,
                vote_average: e.pointer("/rating/average").and_then(Value::as_f64),
            });
        }
        // Absolute numbers over the regular seasons, as for TMDB.
        let mut abs = 0;
        for e in episodes.iter_mut().filter(|e| e.season_number > 0) {
            abs += 1;
            e.absolute_number = Some(abs);
        }
        let mut seasons: Vec<SeasonMetadata> = Vec::new();
        for e in episodes {
            match seasons.iter_mut().find(|s| s.season_number == e.season_number) {
                Some(s) => s.episodes.push(e),
                None => seasons.push(SeasonMetadata {
                    season_number: e.season_number,
                    name: Some(if e.season_number == 0 {
                        "Specials".into()
                    } else {
                        format!("Season {}", e.season_number)
                    }),
                    overview: None,
                    air_date: e.air_date,
                    poster: None,
                    episode_count: 0,
                    episodes: vec![e],
                    tmdb_id: None,
                    tvdb_id: None,
                }),
            }
        }
        seasons.sort_by_key(|s| s.season_number);
        for s in &mut seasons {
            s.episodes.sort_by_key(|e| e.episode_number);
            s.episode_count = s.episodes.len() as u32;
            s.air_date = s.episodes.iter().filter_map(|e| e.air_date).min();
        }
        let today = chrono::Utc::now().date_naive();
        let all = || {
            seasons
                .iter()
                .flat_map(|s| s.episodes.iter())
                .filter(|e| e.season_number > 0)
        };
        let title = str_of(&show, "name").ok_or(MetaError::NotFound)?;
        let first = str_of(&show, "premiered").and_then(|d| parse_date(&d));
        let ext = show.get("externals");
        let tvdb_id = ext.and_then(|x| x.get("thetvdb")).and_then(u32_of_value);
        let imdb_id = ext
            .and_then(|x| x.get("imdb"))
            .and_then(Value::as_str)
            .map(String::from);
        let network = show
            .pointer("/network/name")
            .or_else(|| show.pointer("/webChannel/name"))
            .and_then(Value::as_str)
            .map(String::from);
        let poster = show
            .pointer("/image/original")
            .and_then(Value::as_str)
            .map(String::from);
        Ok(SeriesMetadata {
            tmdb_id,
            tvdb_id,
            imdb_id: imdb_id.clone(),
            sort_title: sort_title(&title),
            original_title: Some(title.clone()),
            overview: str_of(&show, "summary")
                .map(|s| plain(&s))
                .and_then(|s| nonempty(Some(s))),
            year: first.map(|d| chrono::Datelike::year(&d)),
            first_air_date: first,
            last_air_date: all().filter_map(|e| e.air_date).filter(|d| *d <= today).max(),
            next_air_date: all().filter_map(|e| e.air_date).filter(|d| *d > today).min(),
            status: match show.get("status").and_then(Value::as_str) {
                Some("Ended") => SeriesStatus::Ended,
                Some("Running") | Some("To Be Determined") => SeriesStatus::Continuing,
                Some("In Development") => SeriesStatus::Upcoming,
                _ => SeriesStatus::Unknown,
            },
            show_type: str_of(&show, "type"),
            networks: network
                .iter()
                .map(|n| Company {
                    id: 0,
                    name: n.clone(),
                    logo_url: None,
                    origin_country: None,
                })
                .collect(),
            network,
            genres: show
                .get("genres")
                .and_then(Value::as_array)
                .map(|g| {
                    g.iter()
                        .filter_map(Value::as_str)
                        .enumerate()
                        .map(|(i, n)| Genre {
                            id: i as u32 + 1,
                            name: n.into(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            keywords: Vec::new(),
            runtime_minutes: show
                .get("runtime")
                .or_else(|| show.get("averageRuntime"))
                .and_then(u32_of_value),
            certification: None,
            original_language: None,
            origin_country: show
                .pointer("/network/country/code")
                .and_then(Value::as_str)
                .map(|c| vec![c.to_string()])
                .unwrap_or_default(),
            alternative_titles: Vec::new(),
            external_ids: ExternalIds {
                tmdb: tmdb_id,
                imdb: imdb_id,
                tvdb: tvdb_id,
                wikidata: None,
            },
            credits: Credits::default(),
            images: Images {
                poster,
                ..Default::default()
            },
            videos: Vec::new(),
            ratings: Ratings {
                vote_average: show
                    .pointer("/rating/average")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                vote_count: 0,
                popularity: show.get("weight").and_then(Value::as_f64).unwrap_or(0.0),
            },
            title,
            seasons,
            recommendations: Vec::new(),
            similar: Vec::new(),
        })
    }

    fn series_hit(show: &Value) -> Option<SearchHit> {
        let first = str_of(show, "premiered").and_then(|d| parse_date(&d));
        Some(SearchHit {
            kind: MediaKind::Series,
            tmdb_id: None,
            tvdb_id: show.pointer("/externals/thetvdb").and_then(u32_of_value),
            imdb_id: show
                .pointer("/externals/imdb")
                .and_then(Value::as_str)
                .map(String::from),
            title: str_of(show, "name")?,
            original_title: None,
            year: first.map(|d| chrono::Datelike::year(&d)),
            release_date: first,
            overview: str_of(show, "summary").map(|s| plain(&s)),
            poster: show
                .pointer("/image/medium")
                .and_then(Value::as_str)
                .map(String::from),
            backdrop: None,
            genre_ids: Vec::new(),
            original_language: None,
            vote_average: show
                .pointer("/rating/average")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            vote_count: 0,
            popularity: show.get("weight").and_then(Value::as_f64).unwrap_or(0.0),
            adult: false,
        })
    }

    /// Series whose name matches `query`. Only series with a TVDB or IMDb id are returned, as the
    /// library needs one of them.
    pub async fn search_series(&self, query: &str, year: Option<i32>) -> Result<Vec<SearchHit>, MetaError> {
        let url = self.tvmaze("/search/shows");
        let v = self.get(&url, &[("q", query.into())], &self.inner.tvmaze).await?;
        let mut hits: Vec<SearchHit> = v
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| Self::series_hit(r.get("show")?))
            .filter(|h| h.tvdb_id.is_some() || h.imdb_id.is_some())
            .collect();
        if let Some(y) = year {
            hits.retain(|h| h.year.is_none_or(|hy| (hy - y).abs() <= 1));
        }
        Ok(hits)
    }

    /// A series by its TVDB or IMDb id.
    pub async fn find_series(&self, id: SeriesExternalId) -> Result<Vec<SearchHit>, MetaError> {
        match self.show(&id).await {
            Ok((show, tmdb)) => Ok(Self::series_hit(&show)
                .map(|mut h| {
                    h.tmdb_id = tmdb;
                    h
                })
                .into_iter()
                .collect()),
            Err(MetaError::NotFound) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }
}

/// `NotFound` from open data means "not listed there", not "deleted": see [`OpenData::movie`].
fn unlisted(e: MetaError, why: &'static str) -> MetaError {
    match e {
        MetaError::NotFound => MetaError::Unsupported(why),
        other => other,
    }
}

/// A whole number that fits in a `u32`; ids past the range are dropped, never cut short.
fn u32_of_value(v: &Value) -> Option<u32> {
    v.as_u64().and_then(|n| u32::try_from(n).ok())
}

fn u32_of(v: Option<&Value>) -> Option<u32> {
    v.and_then(u32_of_value)
}

/// The most of a response body read: open data answers are far smaller.
const MAX_BODY: usize = 8 * 1024 * 1024;
/// The most of an error response kept for the message shown to people and written to the log.
const MAX_ERROR_TEXT: usize = 200;

/// Reads at most `max` bytes of `resp`'s body; `None` when it is longer.
async fn body_up_to(mut resp: reqwest::Response, max: usize) -> Result<Option<Vec<u8>>, MetaError> {
    if resp.content_length().is_some_and(|n| n > max as u64) {
        return Ok(None);
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > max {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

/// The start of an error body, on one line, at most [`MAX_ERROR_TEXT`] characters.
async fn error_text(mut resp: reqwest::Response) -> String {
    let mut body = Vec::new();
    while body.len() < 4096 {
        match resp.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            _ => break,
        }
    }
    short_text(&String::from_utf8_lossy(&body))
}

fn short_text(text: &str) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match one_line.char_indices().nth(MAX_ERROR_TEXT) {
        Some((at, _)) => format!("{}…", &one_line[..at]),
        None => one_line,
    }
}

/// The page title in a Wikipedia article address, ready to put in a URL path. Wikidata gives the
/// address with `/` left as it is ("Face/Off", "Fahrenheit 9/11"), so everything after `/wiki/`
/// is the title, and its `/`, `?` and `#` are encoded so it stays one path segment.
fn wiki_title(article: &str) -> Option<String> {
    let (_, title) = article.split_once("/wiki/")?;
    let title = title.replace('?', "%3F").replace('#', "%23").replace('/', "%2F");
    (!title.is_empty()).then_some(title)
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(String::from)
}

fn nonempty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// `1991-11-22` or `1991-11-22T00:00:00Z`; a year alone (`1991-00-00`) becomes 1 January.
fn parse_date(s: &str) -> Option<NaiveDate> {
    let d = s.get(..10)?;
    NaiveDate::parse_from_str(d, "%Y-%m-%d").ok().or_else(|| {
        let y: i32 = s.get(..4)?.parse().ok()?;
        NaiveDate::from_ymd_opt(y, 1, 1)
    })
}

fn genres(list: Option<&String>) -> Vec<Genre> {
    list.map(|l| {
        l.split('|')
            .filter(|g| !g.is_empty())
            // Wikidata calls them "comedy film", "horror film": keep the genre word.
            .map(|g| g.trim_end_matches(" film").trim_end_matches(" movie"))
            .enumerate()
            .map(|(i, g)| {
                let mut c = g.chars();
                let name = c
                    .next()
                    .map(|f| f.to_uppercase().chain(c).collect())
                    .unwrap_or_default();
                Genre {
                    id: i as u32 + 1,
                    name,
                }
            })
            .collect()
    })
    .unwrap_or_default()
}

/// TVmaze summaries are HTML: tags dropped, entities for the common characters decoded.
fn plain(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

fn is_qid(s: &str) -> bool {
    s.len() > 1 && s.starts_with('Q') && s[1..].chars().all(|c| c.is_ascii_digit())
}

fn is_imdb(s: &str) -> bool {
    s.len() > 2 && s.starts_with("tt") && s[2..].chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> OpenData {
        OpenData::new(
            reqwest::Client::new(),
            OpenDataOptions {
                sparql_url: format!("{}/sparql", server.uri()),
                wikidata_api_url: format!("{}/w/api.php", server.uri()),
                wikipedia_url: server.uri(),
                tvmaze_url: server.uri(),
                retry: RetryPolicy {
                    max_retries: 0,
                    ..RetryPolicy::default()
                },
                ..Default::default()
            },
        )
    }

    #[test]
    fn wikipedia_titles_keep_their_slashes() {
        assert_eq!(
            wiki_title("https://en.wikipedia.org/wiki/Face/Off").as_deref(),
            Some("Face%2FOff")
        );
        assert_eq!(
            wiki_title("https://en.wikipedia.org/wiki/Fahrenheit_9/11").as_deref(),
            Some("Fahrenheit_9%2F11")
        );
        assert_eq!(
            wiki_title("https://fr.wikipedia.org/wiki/Am%C3%A9lie").as_deref(),
            Some("Am%C3%A9lie")
        );
        assert_eq!(wiki_title("https://en.wikipedia.org/wiki/").as_deref(), None);
        assert_eq!(wiki_title("https://en.wikipedia.org/Off").as_deref(), None);
    }

    #[tokio::test]
    async fn the_summary_of_a_title_with_a_slash_is_that_article() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/rest_v1/page/summary/Face%2FOff"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"extract": "A 1997 film."})),
            )
            .mount(&server)
            .await;
        let article = "https://en.wikipedia.org/wiki/Face/Off".to_string();
        assert_eq!(
            client(&server).summary(Some(&article)).await.as_deref(),
            Some("A 1997 film.")
        );
    }

    #[tokio::test]
    async fn error_text_is_cut_short_and_large_answers_are_refused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/broken"))
            .respond_with(ResponseTemplate::new(503).set_body_string("x".repeat(100_000)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/huge"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!("\"{}\"", "y".repeat(MAX_BODY))))
            .mount(&server)
            .await;
        let c = client(&server);
        match c
            .get(&format!("{}/broken", server.uri()), &[], &c.inner.tvmaze)
            .await
        {
            Err(MetaError::Status { status: 503, message }) => {
                assert!(message.chars().count() <= MAX_ERROR_TEXT + 1, "{}", message.len());
            }
            other => panic!("{other:?}"),
        }
        match c
            .get(&format!("{}/huge", server.uri()), &[], &c.inner.tvmaze)
            .await
        {
            Err(MetaError::Decode(m)) => assert!(m.contains("larger"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ids_past_u32_are_dropped_not_cut_short() {
        assert_eq!(u32_of_value(&Value::from(81189)), Some(81189));
        assert_eq!(u32_of_value(&Value::from(u64::from(u32::MAX) + 5)), None);
        assert_eq!(u32_of(None), None);
    }

    fn binding(pairs: &[(&str, &str)]) -> Value {
        Value::Object(
            pairs
                .iter()
                .map(|(k, v)| {
                    (
                        k.to_string(),
                        serde_json::json!({ "type": "literal", "value": v }),
                    )
                })
                .collect(),
        )
    }

    #[tokio::test]
    async fn a_film_by_its_tmdb_id() {
        let server = MockServer::start().await;
        let rows = vec![
            binding(&[
                ("item", "http://www.wikidata.org/entity/Q1210947"),
                ("label", "The Addams Family"),
                ("tmdb", "2907"),
                ("imdb", "tt0101272"),
                ("date", "1991-12-05T00:00:00Z"),
                ("duration", "99"),
                ("genres", "comedy film|horror film"),
                (
                    "article",
                    "https://en.wikipedia.org/wiki/The_Addams_Family_(1991_film)",
                ),
            ]),
            binding(&[
                ("item", "http://www.wikidata.org/entity/Q1210947"),
                ("date", "1991-11-22T00:00:00Z"),
            ]),
        ];
        Mock::given(method("GET"))
            .and(path("/sparql"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"results": {"bindings": rows}})),
            )
            .mount(&server)
            .await;
        Mock::given(path("/api/rest_v1/page/summary/The_Addams_Family_(1991_film)"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"extract": "A 1991 comedy film."})),
            )
            .mount(&server)
            .await;
        let m = client(&server).movie(2907).await.unwrap();
        assert_eq!(m.title, "The Addams Family");
        assert_eq!(m.sort_title, "addams family");
        assert_eq!(m.imdb_id.as_deref(), Some("tt0101272"));
        assert_eq!(m.release_date, NaiveDate::from_ymd_opt(1991, 11, 22)); // the earliest date
        assert_eq!(m.year, Some(1991));
        assert_eq!(m.runtime_minutes, Some(99));
        assert_eq!(m.status, MovieStatus::Released);
        assert_eq!(
            m.genres.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            ["Comedy", "Horror"]
        );
        assert_eq!(m.overview.as_deref(), Some("A 1991 comedy film."));
        assert_eq!(m.external_ids.wikidata.as_deref(), Some("Q1210947"));
    }

    #[tokio::test]
    async fn an_unknown_film_is_not_found() {
        let server = MockServer::start().await;
        Mock::given(path("/sparql"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"results": {"bindings": []}})),
            )
            .mount(&server)
            .await;
        // Not "not found": a refresh would take that as removed from TMDB and mark the film deleted.
        assert!(matches!(
            client(&server).movie(1).await,
            Err(MetaError::Unsupported(_))
        ));
    }

    fn show() -> Value {
        serde_json::json!({
            "id": 2380, "name": "Blackadder", "type": "Scripted", "status": "Ended", "premiered": "1983-06-15",
            "runtime": 30, "genres": ["Comedy", "History"], "weight": 90,
            "summary": "<p><b>Blackadder</b> follows Edmund &amp; his servant.</p>",
            "rating": {"average": 8.6},
            "network": {"name": "BBC One", "country": {"code": "GB"}},
            "externals": {"thetvdb": 76736, "imdb": "tt0084988"},
            "image": {"medium": "https://img/m.jpg", "original": "https://img/o.jpg"}
        })
    }

    #[tokio::test]
    async fn a_series_by_its_tvdb_id_with_specials() {
        let server = MockServer::start().await;
        Mock::given(path("/lookup/shows"))
            .and(query_param("thetvdb", "76736"))
            .respond_with(ResponseTemplate::new(200).set_body_json(show()))
            .mount(&server)
            .await;
        Mock::given(path("/shows/2380/episodes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"season": 1, "number": 1, "name": "The Foretelling", "airdate": "1983-06-15", "runtime": 35, "summary": "<p>One.</p>"},
                {"season": 1, "number": 2, "name": "Born to Be King", "airdate": "1983-06-22"},
                {"season": 2, "number": 1, "name": "Bells", "airdate": "1986-01-09"},
                {"season": 2, "number": null, "name": "The Cavalier Years", "airdate": "1988-02-05"}
            ])))
            .mount(&server)
            .await;
        Mock::given(path("/sparql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"results": {"bindings": [
                    binding(&[("tmdb", "1186"), ("tvdb", "76736"), ("imdb", "tt0084988")])
                ]}}),
            ))
            .mount(&server)
            .await;
        let s = client(&server)
            .series(SeriesExternalId::Tvdb(76736))
            .await
            .unwrap();
        assert_eq!(s.title, "Blackadder");
        assert_eq!(s.tvdb_id, Some(76736));
        assert_eq!(s.tmdb_id, Some(1186)); // from Wikidata
        assert_eq!(s.imdb_id.as_deref(), Some("tt0084988"));
        assert_eq!(s.status, SeriesStatus::Ended);
        assert_eq!(s.network.as_deref(), Some("BBC One"));
        assert_eq!(
            s.overview.as_deref(),
            Some("Blackadder follows Edmund & his servant.")
        );
        assert_eq!(s.images.poster.as_deref(), Some("https://img/o.jpg"));
        let numbers: Vec<(u32, u32, Option<u32>)> = s
            .episodes()
            .map(|e| (e.season_number, e.episode_number, e.absolute_number))
            .collect();
        assert_eq!(
            numbers,
            [(0, 1, None), (1, 1, Some(1)), (1, 2, Some(2)), (2, 1, Some(3))]
        );
        assert_eq!(s.seasons[0].name.as_deref(), Some("Specials"));
        assert_eq!(s.seasons[1].episodes[0].overview.as_deref(), Some("One."));
    }

    #[tokio::test]
    async fn a_series_known_only_by_its_tmdb_id_goes_through_wikidata() {
        let server = MockServer::start().await;
        Mock::given(path("/sparql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"results": {"bindings": [
                    binding(&[("tvdb", "76736"), ("imdb", "tt0084988")])
                ]}}),
            ))
            .mount(&server)
            .await;
        Mock::given(path("/lookup/shows"))
            .and(query_param("thetvdb", "76736"))
            .respond_with(ResponseTemplate::new(200).set_body_json(show()))
            .mount(&server)
            .await;
        Mock::given(path("/shows/2380/episodes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        let s = client(&server)
            .series(SeriesExternalId::Tmdb(1186))
            .await
            .unwrap();
        assert_eq!((s.tmdb_id, s.tvdb_id), (Some(1186), Some(76736)));
    }

    #[tokio::test]
    async fn a_series_tvmaze_does_not_know_by_tvdb_is_found_by_imdb() {
        let server = MockServer::start().await;
        Mock::given(path("/lookup/shows"))
            .and(query_param("thetvdb", "76736"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(path("/lookup/shows"))
            .and(query_param("imdb", "tt0084988"))
            .respond_with(ResponseTemplate::new(200).set_body_json(show()))
            .mount(&server)
            .await;
        Mock::given(path("/shows/2380/episodes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(path("/sparql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"results": {"bindings": [
                    binding(&[("tmdb", "1186"), ("tvdb", "76736"), ("imdb", "tt0084988")])
                ]}}),
            ))
            .mount(&server)
            .await;
        let s = client(&server)
            .series(SeriesExternalId::Tvdb(76736))
            .await
            .unwrap();
        assert_eq!(s.title, "Blackadder");
    }

    #[tokio::test]
    async fn series_search_keeps_only_matchable_shows() {
        let server = MockServer::start().await;
        Mock::given(path("/search/shows"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"score": 0.9, "show": show()},
                {"score": 0.5, "show": {"id": 9, "name": "Blackadder Rides Again", "externals": {}}}
            ])))
            .mount(&server)
            .await;
        let hits = client(&server).search_series("blackadder", None).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].tvdb_id, hits[0].year), (Some(76736), Some(1983)));
    }

    #[test]
    fn small_helpers() {
        assert_eq!(parse_date("1991-00-00"), NaiveDate::from_ymd_opt(1991, 1, 1));
        assert_eq!(plain("<p>A &amp; B</p>"), "A & B");
        assert!(is_imdb("tt0101272") && !is_imdb("tt01x") && !is_imdb("\" } ?x"));
        assert!(is_qid("Q42") && !is_qid("Q") && !is_qid("P31"));
    }
}
