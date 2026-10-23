use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::HeaderMap;

use crate::config::RateLimitConfig;

pub const CLIENT_IP_HEADER: &str = "x-juuz-client-ip";

const MAX_FAILURES: u32 = 10;
const FAILURE_WINDOW: Duration = Duration::from_secs(300);
const MAX_TRACKED: usize = 20_000;
const IDLE_BUCKET: Duration = Duration::from_secs(120);

struct Bucket {
    tokens: f64,
    updated: Instant,
}

pub struct RateLimiter {
    per_second: f64,
    burst: f64,
    failures: Mutex<HashMap<String, (u32, Instant)>>,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    pub fn new(cfg: &RateLimitConfig) -> RateLimiter {
        RateLimiter {
            per_second: cfg.per_minute as f64 / 60.0,
            burst: cfg.burst.max(1) as f64,
            failures: Mutex::default(),
            buckets: Mutex::default(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.per_second > 0.0
    }

    pub fn allow(&self, key: &str) -> Result<(), u64> {
        if !self.enabled() {
            return Ok(());
        }
        let mut map = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if map.len() >= MAX_TRACKED && !map.contains_key(key) {
            map.retain(|_, b| now.duration_since(b.updated) < IDLE_BUCKET);
            if map.len() >= MAX_TRACKED {
                return Ok(());
            }
        }
        let bucket = map.entry(key.to_string()).or_insert(Bucket {
            tokens: self.burst,
            updated: now,
        });
        let elapsed = now.duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.per_second).min(self.burst);
        bucket.updated = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(((1.0 - bucket.tokens) / self.per_second).ceil().max(1.0) as u64)
        }
    }

    pub fn locked(&self, key: &str) -> bool {
        let mut map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(key) {
            Some((_, started)) if started.elapsed() >= FAILURE_WINDOW => {
                map.remove(key);
                false
            }
            Some((count, _)) => *count >= MAX_FAILURES,
            None => false,
        }
    }

    pub fn fail(&self, key: &str) {
        let mut map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= MAX_TRACKED && !map.contains_key(key) {
            map.retain(|_, (count, started)| {
                started.elapsed() < FAILURE_WINDOW && *count >= MAX_FAILURES
            });
            if map.len() >= MAX_TRACKED {
                return;
            }
        }
        let entry = map.entry(key.to_string()).or_insert((0, Instant::now()));
        if entry.1.elapsed() >= FAILURE_WINDOW {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }
}

pub fn key_for(ip: Option<IpAddr>) -> String {
    match ip {
        Some(IpAddr::V4(v4)) => v4.to_string(),
        Some(IpAddr::V6(v6)) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        None => "unknown".into(),
    }
}

pub fn resolve_ip(
    cfg: &RateLimitConfig,
    peer: Option<IpAddr>,
    headers: &HeaderMap,
) -> Option<IpAddr> {
    if cfg.trust_proxy {
        let forwarded = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok());
        let real = headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<IpAddr>().ok());
        if let Some(ip) = forwarded.or(real) {
            return Some(ip);
        }
    }
    peer
}

pub fn client_key(headers: &HeaderMap) -> String {
    headers
        .get(CLIENT_IP_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(per_minute: u32, burst: u32) -> RateLimiter {
        RateLimiter::new(&RateLimitConfig {
            per_minute,
            burst,
            trust_proxy: false,
        })
    }

    #[test]
    fn locks_an_ip_after_repeated_failures_only() {
        let l = limiter(0, 1);
        for _ in 0..MAX_FAILURES - 1 {
            l.fail("1.1.1.1");
        }
        assert!(!l.locked("1.1.1.1"));
        l.fail("1.1.1.1");
        assert!(l.locked("1.1.1.1"));
        assert!(!l.locked("2.2.2.2"));
    }

    #[test]
    fn bucket_allows_a_burst_then_refuses() {
        let l = limiter(60, 3);
        for _ in 0..3 {
            assert!(l.allow("a").is_ok());
        }
        assert!(l.allow("a").is_err());
        assert!(l.allow("b").is_ok());
    }

    #[test]
    fn zero_rate_disables_the_bucket() {
        let l = limiter(0, 1);
        for _ in 0..100 {
            assert!(l.allow("a").is_ok());
        }
    }

    #[test]
    fn ipv6_clients_share_a_slash_64() {
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        assert_eq!(key_for(Some(a)), key_for(Some(b)));
    }

    #[test]
    fn proxy_headers_only_count_when_trusted() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "9.9.9.9, 3.3.3.3".parse().unwrap());
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let mut cfg = RateLimitConfig::default();
        assert_eq!(resolve_ip(&cfg, Some(peer), &headers), Some(peer));
        cfg.trust_proxy = true;
        assert_eq!(
            resolve_ip(&cfg, Some(peer), &headers),
            Some("3.3.3.3".parse().unwrap())
        );
    }
}
