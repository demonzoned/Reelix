//! Shared HTTP plumbing: sliding-window rate limiting, retry on 429/5xx and a TTL cache.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::{RequestBuilder, Response, StatusCode};
use serde_json::Value;
use tokio::time::Instant;

use crate::MetaError;

/// Allows at most `max` requests in any rolling `period`.
#[derive(Debug)]
pub struct RateLimiter {
    max: usize,
    period: Duration,
    sent: tokio::sync::Mutex<VecDeque<Instant>>,
}

impl RateLimiter {
    pub fn new(max: usize, period: Duration) -> Self {
        Self {
            max: max.max(1),
            period,
            sent: tokio::sync::Mutex::new(VecDeque::new()),
        }
    }

    /// Waits until a request slot is free and claims it.
    pub async fn acquire(&self) {
        let mut sent = self.sent.lock().await;
        loop {
            let now = Instant::now();
            while sent
                .front()
                .is_some_and(|t| now.duration_since(*t) >= self.period)
            {
                sent.pop_front();
            }
            if sent.len() < self.max {
                sent.push_back(now);
                return;
            }
            // Holding the lock while sleeping keeps waiters in FIFO order.
            let wait_until = sent[0] + self.period;
            tokio::time::sleep_until(wait_until).await;
        }
    }
}

/// How failed requests are retried.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Retries after the first attempt (429 and 5xx only).
    pub max_retries: u32,
    /// Backoff when the server sent no `Retry-After`; doubled per attempt.
    pub base_backoff: Duration,
    /// Upper bound for any single wait.
    pub max_wait: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_backoff: Duration::from_millis(500),
            max_wait: Duration::from_secs(30),
        }
    }
}

fn retry_after(resp: &Response) -> Option<Duration> {
    let v = resp.headers().get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    v.trim()
        .parse::<f64>()
        .ok()
        .filter(|s| *s >= 0.0)
        .map(Duration::from_secs_f64)
}

/// Sends a request (rebuilt per attempt), honouring the limiter and retrying 429/5xx.
pub(crate) async fn send(
    build: impl Fn() -> RequestBuilder,
    limiter: Option<&RateLimiter>,
    policy: &RetryPolicy,
) -> Result<Response, MetaError> {
    let mut attempt = 0;
    loop {
        if let Some(l) = limiter {
            l.acquire().await;
        }
        let result = build().send().await;
        let wait = match &result {
            Ok(resp) if resp.status() == StatusCode::TOO_MANY_REQUESTS || resp.status().is_server_error() => {
                retry_after(resp).unwrap_or(policy.base_backoff * 2u32.saturating_pow(attempt))
            }
            Err(e) if e.is_timeout() || e.is_connect() => policy.base_backoff * 2u32.saturating_pow(attempt),
            _ => return result.map_err(MetaError::from),
        };
        if attempt >= policy.max_retries {
            return match result {
                Ok(resp) if resp.status() == StatusCode::TOO_MANY_REQUESTS => Err(MetaError::RateLimited {
                    retry_after_secs: retry_after(&resp).map(|d| d.as_secs()),
                }),
                other => other.map_err(MetaError::from),
            };
        }
        attempt += 1;
        let wait = wait.min(policy.max_wait);
        tracing::debug!(
            attempt,
            wait_ms = wait.as_millis() as u64,
            "metadata request throttled, retrying"
        );
        tokio::time::sleep(wait).await;
    }
}

/// How often expired cache entries are swept while the server is idle.
const SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Small in-memory cache of JSON responses with per-entry expiry.
///
/// Entries are kept as compact JSON text rather than parsed [`Value`] trees: a parsed tree costs
/// several times the text size, and metadata responses (credits, images, whole seasons) are
/// large. Hits re-parse, which is cheap next to the network round-trip they save.
#[derive(Debug)]
pub struct TtlCache {
    max_entries: usize,
    /// Most bytes of JSON text held in all; `usize::MAX` for no limit.
    max_bytes: usize,
    entries: Arc<Entries>,
}

impl TtlCache {
    pub fn new(max_entries: usize) -> Self {
        let entries = Arc::new(Mutex::new(HashMap::new()));
        // Expired entries would otherwise stay resident until the cache fills up again.
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let weak = Arc::downgrade(&entries);
            rt.spawn(async move {
                let mut tick = tokio::time::interval(SWEEP_INTERVAL);
                tick.tick().await;
                loop {
                    tick.tick().await;
                    let Some(entries) = weak.upgrade() else { break };
                    sweep(&entries);
                }
            });
        }
        Self {
            max_entries,
            max_bytes: usize::MAX,
            entries,
        }
    }

    /// Also limits the cache to `max_bytes` of JSON text in all. One answer larger than an
    /// eighth of that is not cached at all, so it cannot push out everything else.
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    pub fn get(&self, key: &str) -> Option<Arc<Value>> {
        let mut map = lock(&self.entries);
        match map.get(key) {
            Some((exp, text)) if *exp > Instant::now() => match serde_json::from_str(text) {
                Ok(v) => Some(Arc::new(v)),
                Err(_) => {
                    map.remove(key);
                    None
                }
            },
            Some(_) => {
                map.remove(key);
                None
            }
            None => None,
        }
    }

    pub fn insert(&self, key: String, value: Arc<Value>, ttl: Duration) {
        if self.max_entries == 0 || ttl.is_zero() {
            return;
        }
        let Ok(text) = serde_json::to_string(&*value) else {
            return;
        };
        if self.max_bytes != usize::MAX && text.len() > self.max_bytes / 8 {
            return;
        }
        let now = Instant::now();
        let mut map = lock(&self.entries);
        if map.len() >= self.max_entries {
            map.retain(|_, (exp, _)| *exp > now);
        }
        if self.max_bytes != usize::MAX {
            map.remove(&key);
            let mut held: usize = map.values().map(|(_, t)| t.len()).sum();
            if held + text.len() > self.max_bytes {
                map.retain(|_, (exp, _)| *exp > now);
                held = map.values().map(|(_, t)| t.len()).sum();
            }
            // Over the limit: drop the entries closest to expiry until the new one fits.
            while held + text.len() > self.max_bytes {
                let Some(k) = map
                    .iter()
                    .min_by_key(|(_, (exp, _))| *exp)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                if let Some((_, t)) = map.remove(&k) {
                    held -= t.len();
                }
            }
        }
        if map.len() >= self.max_entries {
            // Still full: drop the entry closest to expiry.
            if let Some(k) = map
                .iter()
                .min_by_key(|(_, (exp, _))| *exp)
                .map(|(k, _)| k.clone())
            {
                map.remove(&k);
            }
        }
        map.insert(key, (now + ttl, text.into_boxed_str()));
    }

    pub fn clear(&self) {
        lock(&self.entries).clear();
    }

    pub fn len(&self) -> usize {
        lock(&self.entries).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Cache key → (expiry, compact JSON text).
type EntryMap = HashMap<String, (Instant, Box<str>)>;
type Entries = Mutex<EntryMap>;

fn lock(entries: &Entries) -> std::sync::MutexGuard<'_, EntryMap> {
    entries.lock().unwrap_or_else(|e| e.into_inner())
}

/// Drops expired entries and gives the freed capacity back to the allocator.
fn sweep(entries: &Entries) {
    let now = Instant::now();
    let mut map = lock(entries);
    let before = map.len();
    map.retain(|_, (exp, _)| *exp > now);
    if map.len() < before {
        map.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn limiter_spaces_requests_over_the_window() {
        let l = RateLimiter::new(3, Duration::from_secs(10));
        let start = Instant::now();
        for _ in 0..3 {
            l.acquire().await;
        }
        assert!(start.elapsed() < Duration::from_millis(1));
        l.acquire().await;
        assert!(start.elapsed() >= Duration::from_secs(10));
        // 5th and 6th fit in the same second window as the 4th.
        l.acquire().await;
        l.acquire().await;
        assert!(start.elapsed() < Duration::from_secs(11));
        l.acquire().await;
        assert!(start.elapsed() >= Duration::from_secs(20));
    }

    #[tokio::test(start_paused = true)]
    async fn cache_expires_and_evicts() {
        let c = TtlCache::new(2);
        c.insert("a".into(), Arc::new(Value::from(1)), Duration::from_secs(5));
        c.insert("b".into(), Arc::new(Value::from(2)), Duration::from_secs(50));
        assert_eq!(c.get("a").as_deref(), Some(&Value::from(1)));
        c.insert("c".into(), Arc::new(Value::from(3)), Duration::from_secs(50));
        assert_eq!(c.len(), 2);
        assert!(c.get("a").is_none(), "closest-to-expiry entry evicted");
        tokio::time::advance(Duration::from_secs(51)).await;
        assert!(c.get("b").is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn cache_keeps_to_its_byte_limit() {
        let c = TtlCache::new(100).with_max_bytes(80);
        let ten = || Arc::new(Value::from("12345678")); // 10 bytes of JSON text
        c.insert("a".into(), ten(), Duration::from_secs(10));
        c.insert("b".into(), ten(), Duration::from_secs(20));
        assert_eq!(c.len(), 2);
        // Larger than an eighth of the limit: not cached.
        c.insert(
            "big".into(),
            Arc::new(Value::from("x".repeat(20))),
            Duration::from_secs(30),
        );
        assert!(c.get("big").is_none());
        for i in 0..8 {
            c.insert(format!("k{i}"), ten(), Duration::from_secs(30 + i));
        }
        assert_eq!(c.len(), 8, "80 bytes hold eight 10-byte answers");
        assert!(c.get("a").is_none(), "closest-to-expiry entry evicted first");
        assert!(c.get("k7").is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn cache_round_trips_values() {
        let c = TtlCache::new(4);
        let v = serde_json::json!({"id": 7, "title": "Ünïcode", "cast": [{"n": 1.5}, null, true]});
        c.insert("k".into(), Arc::new(v.clone()), Duration::from_secs(60));
        assert_eq!(c.get("k").as_deref(), Some(&v));
    }

    #[tokio::test(start_paused = true)]
    async fn idle_sweep_drops_expired_entries() {
        let c = TtlCache::new(10);
        c.insert("short".into(), Arc::new(Value::from(1)), Duration::from_secs(60));
        c.insert("long".into(), Arc::new(Value::from(2)), SWEEP_INTERVAL * 3);
        tokio::time::sleep(SWEEP_INTERVAL + Duration::from_secs(1)).await;
        assert_eq!(c.len(), 1, "expired entry swept without any cache access");
        assert_eq!(c.get("long").as_deref(), Some(&Value::from(2)));
    }
}
