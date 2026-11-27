//! HTTP plumbing shared by the adapters: per-indexer clients with proxies, the SSRF
//! guard, a tiny cookie jar and the FlareSolverr client.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::redirect;
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

use crate::IndexerError;

/// Largest search/caps/page response read from an indexer.
pub const MAX_PAGE_BYTES: usize = 32 * 1024 * 1024;
/// Largest `.torrent` / `.nzb` download.
pub const MAX_DOWNLOAD_BYTES: usize = 64 * 1024 * 1024;

/// Reads a response body, failing once it exceeds `limit` bytes (a hostile or broken
/// indexer can't make Reelix buffer gigabytes).
pub async fn read_limited(mut resp: reqwest::Response, limit: usize) -> Result<Vec<u8>, IndexerError> {
    if resp.content_length().is_some_and(|n| n > limit as u64) {
        return Err(IndexerError::Parse(format!(
            "response larger than {} MB",
            limit / (1024 * 1024)
        )));
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if out.len() + chunk.len() > limit {
            return Err(IndexerError::Parse(format!(
                "response larger than {} MB",
                limit / (1024 * 1024)
            )));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Proxy kind supported per indexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyKind {
    Http,
    /// SOCKS5 with remote DNS resolution (`socks5h`).
    Socks5,
}

/// An HTTP or SOCKS5 proxy an indexer's traffic is routed through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

impl ProxyConfig {
    /// Proxy URL, e.g. `socks5h://proxy:1080`.
    pub fn url(&self) -> String {
        let scheme = match self.kind {
            ProxyKind::Http => "http",
            ProxyKind::Socks5 => "socks5h",
        };
        format!("{scheme}://{}:{}", self.host, self.port)
    }

    fn to_reqwest(&self) -> Result<reqwest::Proxy, IndexerError> {
        let mut p =
            reqwest::Proxy::all(self.url()).map_err(|e| IndexerError::Config(format!("proxy: {e}")))?;
        if let Some(u) = self.username.as_deref().filter(|u| !u.is_empty()) {
            p = p.basic_auth(u, self.password.as_deref().unwrap_or(""));
        }
        Ok(p)
    }
}

/// Rejects loopback/private/link-local targets unless explicitly allowed.
#[derive(Debug, Clone, Copy)]
pub struct AddressGuard {
    pub allow_private: bool,
    /// The requests go through a proxy, which looks host names up itself (`socks5h`, or an HTTP
    /// proxy's `CONNECT`). The local DNS is then not asked: a site the local resolver blocks may
    /// still be reachable through the proxy, and the address the local DNS gives is not the one
    /// used. Only what can be checked without DNS is checked (IP literals, `localhost`).
    pub remote_dns: bool,
}

/// True for addresses that must not be reachable from user-supplied URLs by default.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // CGNAT 100.64/10
                || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking 198.18/15
                || o[0] == 0 // "this network" 0/8
                || o[0] >= 240 // reserved 240/4
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped `::ffff:a.b.c.d` and IPv4-compatible `::a.b.c.d`.
            if let Some(v4) = v6.to_ipv4() {
                return is_private(IpAddr::V4(v4));
            }
            let s = v6.segments();
            let embedded =
                |hi: u16, lo: u16| IpAddr::V4(Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo)));
            // NAT64 64:ff9b::/96 carries the IPv4 target in the low 32 bits.
            if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                return is_private(embedded(s[6], s[7]));
            }
            // 6to4 2002::/16 carries the IPv4 address in bits 16..48.
            if s[0] == 0x2002 {
                return is_private(embedded(s[1], s[2]));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (s[0] & 0xffc0) == 0xfec0 // deprecated site-local fec0::/10
        }
    }
}

/// Keeps the public addresses of `addrs`, failing when none remain.
fn public_addrs(
    host: &str,
    addrs: impl Iterator<Item = SocketAddr>,
) -> Result<Vec<SocketAddr>, IndexerError> {
    let mut blocked = None;
    let public: Vec<_> = addrs
        .filter(|a| {
            let private = is_private(a.ip());
            if private {
                blocked.get_or_insert(a.ip());
            }
            !private
        })
        .collect();
    if public.is_empty() {
        return Err(IndexerError::Blocked(match blocked {
            Some(ip) => format!("{host} resolves to {ip}"),
            None => format!("cannot resolve {host}"),
        }));
    }
    Ok(public)
}

/// DNS resolver that drops private addresses, so the SSRF guard also holds at connect time
/// (no DNS-rebinding window between [`AddressGuard::check`] and the actual connection, and
/// redirects to host names that resolve privately fail too).
#[derive(Debug, Clone, Copy, Default)]
pub struct GuardedResolver;

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let public = public_addrs(&host, addrs)?;
            Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

impl AddressGuard {
    /// Scheme + literal-host check (no DNS); used at build time and on redirects.
    pub fn check_literal(&self, url: &Url) -> Result<(), IndexerError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(IndexerError::Blocked(format!(
                "unsupported scheme `{}`",
                url.scheme()
            )));
        }
        if self.allow_private {
            return Ok(());
        }
        match url.host() {
            Some(url::Host::Ipv4(ip)) if is_private(ip.into()) => Err(IndexerError::Blocked(ip.to_string())),
            Some(url::Host::Ipv6(ip)) if is_private(ip.into()) => Err(IndexerError::Blocked(ip.to_string())),
            Some(url::Host::Domain(d))
                if d.eq_ignore_ascii_case("localhost") || d.ends_with(".localhost") =>
            {
                Err(IndexerError::Blocked(d.to_string()))
            }
            None => Err(IndexerError::Blocked("missing host".into())),
            _ => Ok(()),
        }
    }

    /// Full check including DNS resolution of host names, unless a proxy resolves them
    /// ([`Self::remote_dns`]).
    pub async fn check(&self, url: &Url) -> Result<(), IndexerError> {
        self.check_literal(url)?;
        if self.allow_private || self.remote_dns {
            return Ok(());
        }
        if let Some(url::Host::Domain(d)) = url.host() {
            let port = url.port_or_known_default().unwrap_or(80);
            let addrs = tokio::net::lookup_host((d, port))
                .await
                .map_err(|e| IndexerError::Blocked(format!("cannot resolve {d}: {e}")))?;
            for a in addrs {
                if is_private(a.ip()) {
                    return Err(IndexerError::Blocked(format!("{d} resolves to {}", a.ip())));
                }
            }
        }
        Ok(())
    }

    /// Enforces the guard inside the connection: unless private addresses are allowed, host
    /// names are resolved through [`GuardedResolver`]. IP literals don't go through DNS, so
    /// pair this with [`Self::redirect_policy`] (or [`Self::check_literal`]) for those.
    pub fn secure(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        if self.allow_private {
            builder
        } else {
            builder.dns_resolver(Arc::new(GuardedResolver))
        }
    }

    /// Follows up to 10 redirects, stopping (the 3xx response is returned) at non-HTTP
    /// schemes such as magnet links and at hops the literal check rejects.
    pub fn redirect_policy(self) -> redirect::Policy {
        redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 10 || self.check_literal(attempt.url()).is_err() {
                attempt.stop()
            } else {
                attempt.follow()
            }
        })
    }
}

/// Whether `host` is `known` or one of its subdomains (a leading `www.` on `known` is
/// ignored). Never true for a parent domain or public suffix of `known`.
pub fn host_within(host: &str, known: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let known = known.trim_end_matches('.').to_ascii_lowercase();
    let known = match known.strip_prefix("www.") {
        Some(rest) if rest.contains('.') => rest,
        _ => known.as_str(),
    };
    !known.is_empty()
        && (host == known
            || host
                .strip_suffix(known)
                .is_some_and(|sub| sub.ends_with('.') && sub.len() > 1))
}

/// Network options for building an indexer's own HTTP client.
#[derive(Debug, Clone)]
pub struct HttpOptions {
    pub proxy: Option<ProxyConfig>,
    pub timeout: Duration,
    pub user_agent: String,
    pub guard: AddressGuard,
}

/// Builds a per-indexer client. Redirects are followed (≤ 10 hops) except into non-HTTP
/// schemes (magnet links surface as the 3xx response) or blocked addresses; host names are
/// resolved through the guard. Behind a proxy the proxy resolves the target, so only the
/// pre-flight [`AddressGuard::check`] applies (and the admin-chosen proxy may be on the LAN).
pub(crate) fn build_client(
    opts: &HttpOptions,
    follow_redirects: bool,
) -> Result<reqwest::Client, IndexerError> {
    let policy = if follow_redirects {
        opts.guard.redirect_policy()
    } else {
        redirect::Policy::none()
    };
    let mut b = reqwest::Client::builder()
        .timeout(opts.timeout)
        .user_agent(opts.user_agent.clone())
        .redirect(policy)
        .gzip(true);
    match &opts.proxy {
        Some(p) => b = b.proxy(p.to_reqwest()?),
        None => b = opts.guard.secure(b),
    }
    b.build()
        .map_err(|e| IndexerError::Config(format!("http client: {e}")))
}

/// Maps non-success statuses to typed errors (401/403 → auth, 429 → request limit).
pub(crate) fn status_error(resp: &reqwest::Response) -> Option<IndexerError> {
    let s = resp.status();
    if s.is_success() || s.is_redirection() {
        return None;
    }
    Some(match s.as_u16() {
        401 | 403 => IndexerError::Auth(format!("HTTP {s}")),
        429 => IndexerError::RequestLimit {
            retry_after: retry_after(resp),
        },
        code => IndexerError::Status { status: code },
    })
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let v = resp.headers().get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    v.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Minimal name → value cookie jar (enough for tracker sessions).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CookieJar(pub Vec<(String, String)>);

impl CookieJar {
    pub fn set(&mut self, name: &str, value: &str) {
        if let Some(c) = self.0.iter_mut().find(|c| c.0 == name) {
            c.1 = value.to_string();
        } else {
            self.0.push((name.to_string(), value.to_string()));
        }
    }

    /// Absorbs every `Set-Cookie` header of a response.
    pub fn absorb(&mut self, headers: &reqwest::header::HeaderMap) {
        for v in headers.get_all(reqwest::header::SET_COOKIE) {
            if let Some((n, val)) = v
                .to_str()
                .ok()
                .and_then(|s| s.split(';').next())
                .and_then(|kv| kv.split_once('='))
            {
                self.set(n.trim(), val.trim());
            }
        }
    }

    /// Parses a raw `a=1; b=2` cookie string (user-supplied cookie logins).
    pub fn parse_header(&mut self, raw: &str) {
        for kv in raw.split(';') {
            if let Some((n, v)) = kv.split_once('=') {
                self.set(n.trim(), v.trim());
            }
        }
    }

    pub fn header(&self) -> Option<String> {
        (!self.0.is_empty()).then(|| {
            self.0
                .iter()
                .map(|(n, v)| format!("{n}={v}"))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// True when a response looks like a Cloudflare browser challenge.
pub(crate) fn is_cloudflare_challenge(status: u16, headers: &reqwest::header::HeaderMap, body: &str) -> bool {
    let cf = headers
        .get(reqwest::header::SERVER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.to_ascii_lowercase().contains("cloudflare"));
    matches!(status, 403 | 503)
        && cf
        && (body.contains("Just a moment")
            || body.contains("cf-challenge")
            || body.contains("challenge-platform"))
}

/// Client for a FlareSolverr instance (`POST /v1`).
#[derive(Debug, Clone)]
pub struct FlareSolverr {
    http: reqwest::Client,
    endpoint: Url,
    max_timeout: Duration,
}

/// A solved page as returned by FlareSolverr.
#[derive(Debug, Clone, PartialEq)]
pub struct Solution {
    pub url: String,
    pub status: u16,
    pub body: String,
    pub cookies: Vec<(String, String)>,
    pub user_agent: Option<String>,
}

#[derive(Deserialize)]
struct FsResponse {
    status: String,
    #[serde(default)]
    message: String,
    solution: Option<FsSolution>,
}

#[derive(Deserialize)]
struct FsSolution {
    url: String,
    #[serde(default)]
    status: u16,
    #[serde(default)]
    response: String,
    #[serde(default)]
    cookies: Vec<FsCookie>,
    #[serde(rename = "userAgent")]
    user_agent: Option<String>,
}

#[derive(Deserialize)]
struct FsCookie {
    name: String,
    value: String,
}

impl FlareSolverr {
    /// `base` is the FlareSolverr root, e.g. `http://flaresolverr:8191/`.
    pub fn new(http: reqwest::Client, base: &Url) -> Result<Self, IndexerError> {
        let endpoint = base
            .join("v1")
            .map_err(|e| IndexerError::Config(format!("flaresolverr url: {e}")))?;
        Ok(Self {
            http,
            endpoint,
            max_timeout: Duration::from_secs(60),
        })
    }

    pub fn with_max_timeout(mut self, d: Duration) -> Self {
        self.max_timeout = d;
        self
    }

    /// Solves a GET (or form POST when `post_data` is set) for `url`.
    pub async fn solve(
        &self,
        url: &str,
        post_data: Option<&str>,
        jar: &CookieJar,
    ) -> Result<Solution, IndexerError> {
        let cookies: Vec<_> = jar
            .0
            .iter()
            .map(|(n, v)| json!({"name": n, "value": v}))
            .collect();
        let mut body = json!({
            "cmd": if post_data.is_some() { "request.post" } else { "request.get" },
            "url": url,
            "maxTimeout": self.max_timeout.as_millis() as u64,
            "cookies": cookies,
        });
        if let Some(p) = post_data {
            body["postData"] = p.into();
        }
        let resp = self
            .http
            .post(self.endpoint.clone())
            .timeout(self.max_timeout + Duration::from_secs(5))
            .json(&body)
            .send()
            .await?;
        // The reply embeds the solved page, so it gets the same cap as a direct page fetch.
        let bytes = read_limited(resp, MAX_PAGE_BYTES).await?;
        let r: FsResponse = serde_json::from_slice(&bytes)
            .map_err(|e| IndexerError::Cloudflare(format!("bad FlareSolverr reply: {e}")))?;
        match (r.status.as_str(), r.solution) {
            ("ok", Some(s)) => Ok(Solution {
                url: s.url,
                status: s.status,
                body: s.response,
                cookies: s.cookies.into_iter().map(|c| (c.name, c.value)).collect(),
                user_agent: s.user_agent,
            }),
            _ => Err(IndexerError::Cloudflare(if r.message.is_empty() {
                r.status
            } else {
                r.message
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn guard_blocks_private_literals() {
        let g = AddressGuard {
            allow_private: false,
            remote_dns: false,
        };
        for u in [
            "http://127.0.0.1/",
            "http://10.1.2.3/",
            "http://[::1]/",
            "http://localhost:9117/",
            "http://100.64.0.1/",
            "ftp://example.com/",
        ] {
            assert!(g.check(&Url::parse(u).unwrap()).await.is_err(), "{u}");
        }
        assert!(
            g.check_literal(&Url::parse("https://93.184.216.34/").unwrap())
                .is_ok()
        );
        assert!(
            AddressGuard {
                allow_private: true,
                remote_dns: false
            }
            .check_literal(&Url::parse("http://192.168.1.2/").unwrap())
            .is_ok()
        );
    }

    /// behind a proxy the local DNS is not asked, but literals are still checked.
    #[tokio::test]
    async fn guard_behind_a_proxy_leaves_names_to_the_proxy() {
        let local = AddressGuard {
            allow_private: false,
            remote_dns: false,
        };
        let proxied = AddressGuard {
            allow_private: false,
            remote_dns: true,
        };
        // `.invalid` never resolves (RFC 6761), like a site the local provider blocks by DNS.
        let blocked = Url::parse("https://indexer.invalid/api").unwrap();
        assert!(matches!(
            local.check(&blocked).await,
            Err(IndexerError::Blocked(m)) if m.contains("cannot resolve")
        ));
        assert!(proxied.check(&blocked).await.is_ok());
        for u in [
            "http://127.0.0.1/",
            "http://10.1.2.3/",
            "http://[::1]/",
            "http://localhost:9117/",
            "http://tracker.localhost/",
            "ftp://example.com/",
        ] {
            assert!(proxied.check(&Url::parse(u).unwrap()).await.is_err(), "{u}");
        }
    }

    #[test]
    fn host_within_never_matches_parents() {
        assert!(host_within("tracker.example", "tracker.example"));
        assert!(host_within("dl.tracker.example", "tracker.example"));
        assert!(host_within("tracker.example", "www.tracker.example"));
        assert!(host_within("TRACKER.example.", "tracker.example"));
        assert!(!host_within("example", "tracker.example"));
        assert!(!host_within("evil.example", "tracker.example"));
        assert!(!host_within("co.uk", "tracker.co.uk"));
        assert!(!host_within("eviltracker.example", "tracker.example"));
        assert!(!host_within(".tracker.example", "tracker.example"));
    }

    #[test]
    fn private_ranges() {
        for ip in [
            "0.1.2.3",
            "100.64.0.1",
            "198.18.0.1",
            "198.19.255.254",
            "224.0.0.1",
            "239.255.255.250",
            "255.255.255.255",
            "240.0.0.1",
            "::ffff:127.0.0.1",
            "::127.0.0.1",
            "::10.0.0.1",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "2002:c0a8:0101::1",
            "2002:7f00:1::",
            "ff02::1",
            "fd00::1",
            "fe80::1",
        ] {
            assert!(is_private(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "93.184.216.34",
            "198.20.0.1",
            "100.128.0.1",
            "::ffff:93.184.216.34",
            "64:ff9b::5db8:d822",
            "2002:5db8:d822::1",
            "2606:4700::1111",
        ] {
            assert!(!is_private(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn resolver_drops_private_addresses() {
        let a = |s: &str| s.parse::<SocketAddr>().unwrap();
        let kept = public_addrs(
            "mixed.test",
            [a("127.0.0.1:80"), a("93.184.216.34:80"), a("[fd00::1]:80")].into_iter(),
        )
        .unwrap();
        assert_eq!(kept, vec![a("93.184.216.34:80")]);
        assert!(matches!(
            public_addrs("lan.test", [a("10.0.0.1:80"), a("[::1]:80")].into_iter()),
            Err(IndexerError::Blocked(m)) if m.contains("10.0.0.1")
        ));
    }

    #[tokio::test]
    async fn guarded_resolver_refuses_localhost() {
        use reqwest::dns::Resolve;
        let name: reqwest::dns::Name = "localhost".parse().unwrap();
        assert!(GuardedResolver.resolve(name).await.is_err());
    }

    #[tokio::test]
    async fn guarded_client_cannot_reach_loopback_by_name() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let opts = |allow_private| HttpOptions {
            proxy: None,
            timeout: Duration::from_secs(5),
            user_agent: "t".into(),
            guard: AddressGuard {
                allow_private,
                remote_dns: false,
            },
        };
        let url = format!("http://localhost:{port}/");
        let err = build_client(&opts(false), true)
            .unwrap()
            .get(&url)
            .send()
            .await
            .unwrap_err();
        assert!(err.is_connect(), "{err:?}");
        drop(listener);
    }

    #[test]
    fn cookie_jar() {
        let mut j = CookieJar::default();
        j.parse_header("uid=1; pass=abc");
        j.set("uid", "2");
        assert_eq!(j.header().as_deref(), Some("uid=2; pass=abc"));
    }

    #[test]
    fn proxy_url() {
        let p = ProxyConfig {
            kind: ProxyKind::Socks5,
            host: "p".into(),
            port: 1080,
            username: None,
            password: None,
        };
        assert_eq!(p.url(), "socks5h://p:1080");
        assert!(p.to_reqwest().is_ok());
    }
}
