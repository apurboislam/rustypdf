//! Service configuration, read from environment variables (PLAN.md §9).

use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub templates_dir: PathBuf,
    pub fonts_dir: PathBuf,
    pub api_keys: Vec<String>,
    pub required_fonts: Vec<String>,
    pub max_concurrency: usize,
    pub queue_timeout: Duration,
    pub render_timeout: Duration,
    pub max_body_bytes: usize,
    pub image_fetch_timeout: Duration,
    pub image_max_bytes: u64,
    pub image_max_per_request: usize,
    pub image_fetch_concurrency: usize,
    pub image_cache_mb: u64,
    pub image_allowed_hosts: Vec<String>,
    /// Evict Typst's memo cache after this many renders (`crate::memo`).
    pub memo_evict_every: u64,
    pub hot_reload: bool,
    pub preview: bool,
}

impl Config {
    /// Reads the configuration from the process environment.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| env::var(key).ok())
    }

    /// Reads the configuration through `lookup`, so tests need not touch the
    /// process environment.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |key: &str| {
            lookup(key)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let num = |key: &str, default: u64| -> Result<u64> {
            match get(key) {
                Some(v) => v
                    .parse()
                    .with_context(|| format!("{key}={v:?} is not a number")),
                None => Ok(default),
            }
        };
        let list = |key: &str| -> Vec<String> {
            get(key)
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default()
        };
        let flag = |key: &str, default: bool| -> Result<bool> {
            match get(key).as_deref() {
                None => Ok(default),
                Some("1" | "true" | "yes" | "on") => Ok(true),
                Some("0" | "false" | "no" | "off") => Ok(false),
                Some(v) => bail!("{key}={v:?} must be true or false"),
            }
        };
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());

        let config = Self {
            port: u16::try_from(num("PORT", 8080)?).context("PORT is out of range")?,
            templates_dir: dir_or_dev_default(get("TEMPLATES_DIR"), "/templates", "templates"),
            fonts_dir: dir_or_dev_default(get("FONTS_DIR"), "/fonts", "fonts"),
            api_keys: list("API_KEYS"),
            required_fonts: match get("REQUIRED_FONTS") {
                Some(_) => list("REQUIRED_FONTS"),
                None => vec!["Noto Sans Bengali".into(), "Inter".into()],
            },
            max_concurrency: num("MAX_CONCURRENCY", cores as u64)? as usize,
            queue_timeout: Duration::from_millis(num("QUEUE_TIMEOUT_MS", 5000)?),
            render_timeout: Duration::from_millis(num("RENDER_TIMEOUT_MS", 10_000)?),
            max_body_bytes: num("MAX_BODY_KB", 1024)? as usize * 1024,
            image_fetch_timeout: Duration::from_millis(num("IMAGE_FETCH_TIMEOUT_MS", 3000)?),
            image_max_bytes: num("IMAGE_MAX_BYTES", 5 * 1024 * 1024)?,
            image_max_per_request: num("IMAGE_MAX_PER_REQUEST", 200)? as usize,
            image_fetch_concurrency: num("IMAGE_FETCH_CONCURRENCY", 16)? as usize,
            image_cache_mb: num("IMAGE_CACHE_MB", 256)?,
            image_allowed_hosts: list("IMAGE_ALLOWED_HOSTS")
                .into_iter()
                .map(|h| h.to_ascii_lowercase())
                .collect(),
            memo_evict_every: num("MEMO_EVICT_EVERY", crate::memo::DEFAULT_EVICT_EVERY)?,
            hot_reload: flag("HOT_RELOAD", true)?,
            preview: flag("PREVIEW", true)?,
        };
        if config.max_concurrency == 0 {
            bail!("MAX_CONCURRENCY must be at least 1");
        }
        if config.memo_evict_every == 0 {
            bail!("MEMO_EVICT_EVERY must be at least 1");
        }
        Ok(config)
    }
}

/// Uses `value` if set. Otherwise uses the container path `prod`, or the
/// repo-relative `dev` path when `prod` does not exist (local development).
fn dir_or_dev_default(value: Option<String>, prod: &str, dev: &str) -> PathBuf {
    match value {
        Some(v) => PathBuf::from(v),
        None if !Path::new(prod).exists() && Path::new(dev).exists() => PathBuf::from(dev),
        None => PathBuf::from(prod),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> Result<Config> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|k| map.get(k).cloned())
    }

    #[test]
    fn defaults() {
        let c = config(&[]).unwrap();
        assert_eq!(c.port, 8080);
        assert_eq!(c.required_fonts, ["Noto Sans Bengali", "Inter"]);
        assert!(c.api_keys.is_empty());
        assert!(c.image_allowed_hosts.is_empty());
        assert_eq!(c.render_timeout, Duration::from_secs(10));
        assert!(c.hot_reload);
        assert!(c.preview);
        assert!(c.max_concurrency >= 1);
    }

    #[test]
    fn lists_are_trimmed() {
        let c = config(&[
            ("API_KEYS", " a, b ,,"),
            ("IMAGE_ALLOWED_HOSTS", "CDN.example.com, *.s3.amazonaws.com"),
        ])
        .unwrap();
        assert_eq!(c.api_keys, ["a", "b"]);
        assert_eq!(
            c.image_allowed_hosts,
            ["cdn.example.com", "*.s3.amazonaws.com"]
        );
    }

    #[test]
    fn bad_values_are_errors() {
        assert!(config(&[("PORT", "x")]).is_err());
        assert!(config(&[("MAX_CONCURRENCY", "0")]).is_err());
        assert!(config(&[("MEMO_EVICT_EVERY", "0")]).is_err());
        assert!(config(&[("HOT_RELOAD", "maybe")]).is_err());
        assert!(config(&[("PREVIEW", "maybe")]).is_err());
        assert!(!config(&[("PREVIEW", "off")]).unwrap().preview);
    }
}
