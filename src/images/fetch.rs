//! URL image fetching: SSRF guard, optional host allowlist, size and time
//! caps, and an LRU cache of the (resized) results.
//!
//! The SSRF guard has two halves, and both run on every redirect hop:
//! - [`check_url`] rejects non-HTTP schemes, hosts outside the allowlist and
//!   IP-literal hosts in blocked ranges;
//! - [`GuardedResolver`] resolves host names and fails when any address is in
//!   a blocked range, so DNS cannot point the fetch at an internal service.
//!
//! The client never uses a proxy: a proxy would resolve names itself and
//! bypass the resolver check.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use moka::Expiry;
use moka::future::Cache;
use reqwest::Url;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use typst::foundations::Bytes;

use super::assets::Image;
use super::resize;
use crate::config::Config;
use crate::metrics::FetchMetrics;

/// Redirect hops followed per fetch.
const MAX_REDIRECTS: usize = 3;
/// Cache TTL when the response has no usable `Cache-Control: max-age`.
const DEFAULT_TTL: Duration = Duration::from_secs(3600);
/// Upper bound on a `max-age` TTL.
const MAX_TTL: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, thiserror::Error)]
pub enum FetchError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("only http and https URLs are allowed")]
    Scheme,
    #[error("host {0:?} is not in IMAGE_ALLOWED_HOSTS")]
    HostNotAllowed(String),
    #[error("address {0} is private, loopback, link-local or otherwise blocked")]
    BlockedAddress(IpAddr),
    #[error("too many redirects (max {MAX_REDIRECTS})")]
    TooManyRedirects,
    #[error("timeout")]
    Timeout,
    #[error("HTTP {0}")]
    Status(u16),
    #[error("image exceeds {0} bytes")]
    TooLarge(u64),
    #[error("not a PNG, JPEG, GIF, WebP or SVG image")]
    NotImage,
    #[error("{0}")]
    Image(String),
    #[error("{0}")]
    Network(String),
}

pub struct Fetcher {
    client: reqwest::Client,
    cache: Cache<(String, Option<u32>), Arc<Entry>>,
    allowed_hosts: Arc<Vec<String>>,
    timeout: Duration,
    max_bytes: u64,
    /// Fetch latency and cache hit/miss counters.
    pub metrics: FetchMetrics,
}

struct Entry {
    image: Image,
    ttl: Duration,
}

struct EntryExpiry;

impl Expiry<(String, Option<u32>), Arc<Entry>> for EntryExpiry {
    fn expire_after_create(
        &self,
        _key: &(String, Option<u32>),
        value: &Arc<Entry>,
        _created_at: std::time::Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

impl Fetcher {
    pub fn new(config: &Config) -> Self {
        Self::build(config, &[])
    }

    /// Like [`Fetcher::new`], but `host -> address` overrides skip DNS and
    /// the resolver check. Only for tests and the dev `render --resolve`
    /// flag, which point at a local mock server; never set from service
    /// config. IP-literal URLs and every other host stay guarded.
    pub fn with_dns_overrides(config: &Config, overrides: &[(&str, SocketAddr)]) -> Self {
        Self::build(config, overrides)
    }

    fn build(config: &Config, overrides: &[(&str, SocketAddr)]) -> Self {
        let allowed_hosts = Arc::new(config.image_allowed_hosts.clone());
        let policy_hosts = allowed_hosts.clone();
        let redirect = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > MAX_REDIRECTS {
                attempt.error(FetchError::TooManyRedirects)
            } else if let Err(err) = check_url(attempt.url(), &policy_hosts) {
                attempt.error(err)
            } else {
                attempt.follow()
            }
        });
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(redirect)
            .dns_resolver(Arc::new(GuardedResolver))
            .connect_timeout(config.image_fetch_timeout)
            .user_agent(concat!("rustypdf/", env!("CARGO_PKG_VERSION")));
        for (host, addr) in overrides {
            builder = builder.resolve(host, *addr);
        }
        let client = builder.build().expect("static HTTP client configuration");

        let budget = config.image_cache_mb.saturating_mul(1024 * 1024);
        let cache = Cache::builder()
            .max_capacity(budget)
            .weigher(|_k, v: &Arc<Entry>| u32::try_from(v.image.bytes.len()).unwrap_or(u32::MAX))
            .expire_after(EntryExpiry)
            .build();
        Self {
            client,
            cache,
            allowed_hosts,
            timeout: config.image_fetch_timeout,
            max_bytes: config.image_max_bytes,
            metrics: FetchMetrics::default(),
        }
    }

    /// Fetches `url`, checks it is an image and downscales it to `max_px`.
    /// Results are cached by URL + `max_px`; identical concurrent fetches
    /// share one request.
    pub async fn fetch(&self, url: &str, max_px: Option<u32>) -> Result<Image, FetchError> {
        let key = (url.to_owned(), max_px);
        if let Some(entry) = self.cache.get(&key).await {
            self.metrics.cache_hits.inc();
            return Ok(entry.image.clone());
        }
        self.metrics.cache_misses.inc();
        let entry = self
            .cache
            .try_get_with(key, async {
                let start = Instant::now();
                let downloaded = self.download(url).await;
                self.metrics
                    .fetch_seconds
                    .with_label_values(&[if downloaded.is_ok() { "ok" } else { "error" }])
                    .observe(start.elapsed().as_secs_f64());
                let (bytes, ttl) = downloaded?;
                let mut image = Image::new(bytes).ok_or(FetchError::NotImage)?;
                if let Some(max_px) = max_px {
                    image = tokio::task::spawn_blocking(move || resize::downscale(image, max_px))
                        .await
                        .map_err(|e| FetchError::Image(e.to_string()))?
                        .map_err(FetchError::Image)?;
                }
                Ok(Arc::new(Entry { image, ttl }))
            })
            .await
            .map_err(|e: Arc<FetchError>| (*e).clone())?;
        Ok(entry.image.clone())
    }

    /// Downloads the body, enforcing the timeout and size cap. Returns the
    /// bytes and the cache TTL from `Cache-Control`.
    async fn download(&self, url: &str) -> Result<(Bytes, Duration), FetchError> {
        let url = Url::parse(url).map_err(|e| FetchError::InvalidUrl(e.to_string()))?;
        check_url(&url, &self.allowed_hosts)?;
        let deadline = Instant::now() + self.timeout;
        let work = async {
            let mut response = self
                .client
                .get(url)
                .send()
                .await
                .map_err(map_reqwest_error)?;
            let status = response.status();
            if !status.is_success() {
                return Err(FetchError::Status(status.as_u16()));
            }
            if response
                .content_length()
                .is_some_and(|n| n > self.max_bytes)
            {
                return Err(FetchError::TooLarge(self.max_bytes));
            }
            let ttl = cache_ttl(response.headers());
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(map_reqwest_error)? {
                if (body.len() + chunk.len()) as u64 > self.max_bytes {
                    return Err(FetchError::TooLarge(self.max_bytes));
                }
                body.extend_from_slice(&chunk);
            }
            Ok((Bytes::new(body), ttl))
        };
        tokio::time::timeout_at(deadline.into(), work)
            .await
            .map_err(|_| FetchError::Timeout)?
    }

    /// Number of cached images.
    pub fn cached_entries(&self) -> u64 {
        self.cache.entry_count()
    }
}

/// Finds our own error inside a reqwest error chain (raised by the redirect
/// policy or the resolver), so the caller sees "blocked address" rather than
/// a generic connect error.
fn map_reqwest_error(err: reqwest::Error) -> FetchError {
    if err.is_timeout() {
        return FetchError::Timeout;
    }
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&err);
    while let Some(e) = source {
        if let Some(ours) = e.downcast_ref::<FetchError>() {
            return ours.clone();
        }
        source = e.source();
    }
    // The innermost cause is the useful part ("Connection refused",
    // "failed to lookup address information: ...").
    let mut root: &(dyn std::error::Error + 'static) = &err;
    while let Some(e) = root.source() {
        root = e;
    }
    let kind = if err.is_connect() {
        "connect"
    } else {
        "request"
    };
    FetchError::Network(format!("{kind} failed: {root}"))
}

/// TTL from `Cache-Control`: `no-store`/`no-cache` mean zero, `max-age=N`
/// means N seconds (at most a day), anything else the default hour.
fn cache_ttl(headers: &reqwest::header::HeaderMap) -> Duration {
    let Some(value) = headers
        .get(reqwest::header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
    else {
        return DEFAULT_TTL;
    };
    let mut ttl = DEFAULT_TTL;
    for directive in value.split(',').map(|d| d.trim().to_ascii_lowercase()) {
        if directive == "no-store" || directive == "no-cache" {
            return Duration::ZERO;
        }
        if let Some(secs) = directive.strip_prefix("max-age=")
            && let Ok(secs) = secs.trim_matches('"').parse::<u64>()
        {
            ttl = Duration::from_secs(secs).min(MAX_TTL);
        }
    }
    ttl
}

/// Checks the scheme, the allowlist and IP-literal hosts. Runs before the
/// first request and on every redirect hop.
pub fn check_url(url: &Url, allowed_hosts: &[String]) -> Result<(), FetchError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchError::Scheme);
    }
    let host = url
        .host()
        .ok_or_else(|| FetchError::InvalidUrl("URL has no host".into()))?;
    let name = match host {
        url::Host::Domain(d) => d.trim_end_matches('.').to_ascii_lowercase(),
        url::Host::Ipv4(ip) => ip.to_string(),
        url::Host::Ipv6(ip) => ip.to_string(),
    };
    if !host_allowed(&name, allowed_hosts) {
        return Err(FetchError::HostNotAllowed(name));
    }
    let ip = match host {
        url::Host::Domain(_) => None,
        url::Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
    };
    match ip {
        Some(ip) if is_blocked(ip) => Err(FetchError::BlockedAddress(ip)),
        _ => Ok(()),
    }
}

/// Whether `host` (lowercase) matches the allowlist. An empty list allows
/// every host. `*.example.com` matches any subdomain of example.com, but not
/// example.com itself.
pub fn host_allowed(host: &str, allowed: &[String]) -> bool {
    allowed.is_empty()
        || allowed
            .iter()
            .any(|pattern| match pattern.strip_prefix("*.") {
                Some(suffix) => host
                    .strip_suffix(suffix)
                    .is_some_and(|rest| rest.len() > 1 && rest.ends_with('.')),
                None => host == pattern,
            })
}

/// Private, loopback, link-local (incl. cloud metadata), CGNAT, multicast,
/// documentation, benchmarking and reserved ranges.
pub fn is_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_blocked_v4(ip),
        IpAddr::V6(ip) => is_blocked_v6(ip),
    }
}

fn is_blocked_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // 100.64/10 shared (CGNAT)
        || (a == 192 && b == 0 && c == 0) // 192.0.0/24 IETF protocol assignments
        || (a == 198 && (18..20).contains(&b)) // 198.18/15 benchmarking
        || a >= 240 // 240/4 reserved
}

fn is_blocked_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_blocked_v4(v4);
    }
    let seg = ip.segments();
    // 64:ff9b::/96 NAT64 embeds an IPv4 address.
    if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [a, b] = seg[6].to_be_bytes();
        let [c, d] = seg[7].to_be_bytes();
        return is_blocked_v4(Ipv4Addr::new(a, b, c, d));
    }
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
        || (seg[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        || (seg[0] & 0xffc0) == 0xfec0 // fec0::/10 site-local (deprecated)
        || (seg[0] == 0x2001 && seg[1] == 0x0db8) // documentation
        || seg[..6] == [0, 0, 0, 0, 0, 0] // ::/96 IPv4-compatible (deprecated)
}

/// System DNS, failing when any resolved address is blocked.
struct GuardedResolver;

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            if let Some(bad) = addrs.iter().find(|a| is_blocked(a.ip())) {
                return Err(Box::new(FetchError::BlockedAddress(bad.ip())) as _);
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_ranges() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.100.100.200",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "::",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "::127.0.0.1",
        ] {
            assert!(is_blocked(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["93.184.216.34", "8.8.8.8", "172.32.0.1", "2606:4700::1111"] {
            assert!(!is_blocked(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn allowlist() {
        let list = vec![
            "cdn.example.com".to_owned(),
            "*.s3.amazonaws.com".to_owned(),
        ];
        assert!(host_allowed("anything.org", &[]));
        assert!(host_allowed("cdn.example.com", &list));
        assert!(host_allowed("bucket.s3.amazonaws.com", &list));
        assert!(host_allowed("a.b.s3.amazonaws.com", &list));
        assert!(!host_allowed("s3.amazonaws.com", &list));
        assert!(!host_allowed("evils3.amazonaws.com", &list));
        assert!(!host_allowed("other.example.com", &list));
    }

    #[test]
    fn urls_are_checked() {
        let check = |u: &str, hosts: &[&str]| {
            let hosts: Vec<String> = hosts.iter().map(|h| h.to_string()).collect();
            check_url(&Url::parse(u).unwrap(), &hosts)
        };
        assert!(check("https://cdn.example.com/a.png", &[]).is_ok());
        assert!(check("https://CDN.Example.com./a.png", &["cdn.example.com"]).is_ok());
        assert!(matches!(
            check("ftp://x/a.png", &[]),
            Err(FetchError::Scheme)
        ));
        assert!(matches!(
            check("file:///etc/passwd", &[]),
            Err(FetchError::Scheme)
        ));
        assert!(matches!(
            check("http://127.0.0.1/", &[]),
            Err(FetchError::BlockedAddress(_))
        ));
        assert!(matches!(
            check("http://[::1]:8080/", &[]),
            Err(FetchError::BlockedAddress(_))
        ));
        assert!(matches!(
            check("http://169.254.169.254/latest/meta-data", &[]),
            Err(FetchError::BlockedAddress(_))
        ));
        assert!(matches!(
            check("https://evil.com/a.png", &["cdn.example.com"]),
            Err(FetchError::HostNotAllowed(_))
        ));
    }

    #[test]
    fn ttl_follows_cache_control() {
        let ttl = |v: &str| {
            let mut h = reqwest::header::HeaderMap::new();
            h.insert(reqwest::header::CACHE_CONTROL, v.parse().unwrap());
            cache_ttl(&h)
        };
        assert_eq!(cache_ttl(&Default::default()), DEFAULT_TTL);
        assert_eq!(ttl("public, max-age=60"), Duration::from_secs(60));
        assert_eq!(ttl("max-age=99999999"), MAX_TTL);
        assert_eq!(ttl("no-store"), Duration::ZERO);
        assert_eq!(ttl("private"), DEFAULT_TTL);
    }
}
