use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, params};
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

use crate::config::PodcastConfig;
use crate::db::{Db, now};
use crate::ids;

const MAX_FEED_BYTES: u64 = 10 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

pub struct Fetcher {
    agent: ureq::Agent,
    allow_private: bool,
    pub max_episode_size: u64,
}

fn public_v4(v4: std::net::Ipv4Addr) -> bool {
    let o = v4.octets();
    !(v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || v4.is_documentation()
        || o[0] == 0
        || o[0] >= 240
        || (o[0] == 100 && (64..128).contains(&o[1]))
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)))
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            let embedded = |hi: u16, lo: u16| {
                let [a, b] = hi.to_be_bytes();
                let [c, d] = lo.to_be_bytes();
                std::net::Ipv4Addr::new(a, b, c, d)
            };
            if v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00
                || (seg[0] & 0xffc0) == 0xfe80
                || (seg[0] & 0xffc0) == 0xfec0
                || (seg[0] == 0x2001 && (seg[1] == 0 || seg[1] == 0x0db8))
                || (seg[0] == 0x0064 && seg[1] == 0xff9b && seg[2] == 1)
            {
                return false;
            }
            if let Some(v4) = v6.to_ipv4() {
                return public_v4(v4);
            }
            if seg[0] == 0x0064 && seg[1] == 0xff9b && seg[2..6].iter().all(|s| *s == 0) {
                return public_v4(embedded(seg[6], seg[7]));
            }
            if seg[0] == 0x2002 {
                return public_v4(embedded(seg[1], seg[2]));
            }
            true
        }
    }
}

#[derive(Debug)]
struct PublicResolver {
    inner: DefaultResolver,
}

impl Resolver for PublicResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let addrs = self.inner.resolve(uri, config, timeout)?;
        let mut allowed = self.empty();
        for addr in addrs.iter().filter(|a| is_public(a.ip())) {
            allowed.push(*addr);
        }
        if allowed.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(allowed)
    }
}

impl Fetcher {
    pub fn new(cfg: &PodcastConfig) -> Fetcher {
        let mut builder = ureq::Agent::config_builder();
        if !cfg.allow_private_hosts {
            builder = builder.proxy(None);
        }
        let config = builder
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_body(Some(Duration::from_secs(600)))
            .timeout_global(Some(Duration::from_secs(3600)))
            .user_agent(concat!("juuz/", env!("CARGO_PKG_VERSION")))
            .build();
        let agent = if cfg.allow_private_hosts {
            ureq::Agent::new_with_config(config)
        } else {
            ureq::Agent::with_parts(
                config,
                DefaultConnector::default(),
                PublicResolver {
                    inner: DefaultResolver::default(),
                },
            )
        };
        Fetcher {
            agent,
            allow_private: cfg.allow_private_hosts,
            max_episode_size: cfg.max_episode_size,
        }
    }

    fn check(&self, url: &str) -> Result<()> {
        let uri: ureq::http::Uri = url.parse().map_err(|_| anyhow!("invalid url"))?;
        let scheme = uri.scheme_str().unwrap_or("");
        if scheme != "http" && scheme != "https" {
            bail!("only http and https urls are supported");
        }
        let host = uri.host().ok_or_else(|| anyhow!("url has no host"))?;
        if self.allow_private {
            return Ok(());
        }
        let port = uri
            .port_u16()
            .unwrap_or(if scheme == "https" { 443 } else { 80 });
        let addrs: Vec<_> = (host.trim_matches(['[', ']']), port)
            .to_socket_addrs()
            .map_err(|e| anyhow!("cannot resolve {host}: {e}"))?
            .collect();
        if addrs.is_empty() || addrs.iter().any(|a| !is_public(a.ip())) {
            bail!("host {host} is not a public address");
        }
        Ok(())
    }

    fn get(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>> {
        let mut current = url.to_string();
        for _ in 0..=MAX_REDIRECTS {
            self.check(&current)?;
            let resp = self.agent.get(&current).call()?;
            let status = resp.status().as_u16();
            if (300..400).contains(&status) {
                let location = resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| anyhow!("redirect without location"))?;
                let base: ureq::http::Uri = current.parse()?;
                current = resolve(&base, location)?;
                continue;
            }
            if status != 200 {
                bail!("server returned HTTP {status}");
            }
            return Ok(resp);
        }
        bail!("too many redirects")
    }

    pub fn fetch_feed(&self, url: &str) -> Result<feed_rs::model::Feed> {
        let mut resp = self.get(url)?;
        let mut buf = Vec::new();
        resp.body_mut()
            .as_reader()
            .take(MAX_FEED_BYTES + 1)
            .read_to_end(&mut buf)?;
        if buf.len() as u64 > MAX_FEED_BYTES {
            bail!("feed is too large");
        }
        Ok(feed_rs::parser::parse(&buf[..])?)
    }

    pub fn download(&self, url: &str, dest: &Path) -> Result<(u64, Option<String>)> {
        let mut resp = self.get(url)?;
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_string());
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = dest.with_extension("part");
        let result = (|| -> Result<u64> {
            let mut file = std::fs::File::create(&tmp)?;
            let mut reader = resp.body_mut().as_reader().take(self.max_episode_size + 1);
            let copied = std::io::copy(&mut reader, &mut file)?;
            if copied > self.max_episode_size {
                bail!("episode exceeds the size limit");
            }
            Ok(copied)
        })();
        match result {
            Ok(size) => {
                std::fs::rename(&tmp, dest)?;
                Ok((size, content_type))
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
}

fn resolve(base: &ureq::http::Uri, location: &str) -> Result<String> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return Ok(location.to_string());
    }
    let scheme = base.scheme_str().unwrap_or("http");
    let authority = base
        .authority()
        .map(|a| a.as_str().to_string())
        .ok_or_else(|| anyhow!("redirect base has no host"))?;
    if location.starts_with('/') {
        return Ok(format!("{scheme}://{authority}{location}"));
    }
    let dir = base.path().rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    Ok(format!("{scheme}://{authority}{dir}/{location}"))
}

pub fn create_channel(conn: &Connection, url: &str) -> Result<String> {
    let id = ids::random();
    conn.execute(
        "INSERT INTO podcast_channels(id, url, created_at) VALUES (?1, ?2, ?3)",
        params![id, url, now()],
    )?;
    Ok(id)
}

fn text(t: &Option<feed_rs::model::Text>) -> Option<String> {
    t.as_ref()
        .map(|t| t.content.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn refresh_channel(db: &Db, fetcher: &Fetcher, id: &str) {
    let conn = match db.conn() {
        Ok(c) => c,
        Err(_) => return,
    };
    let url: Option<String> = conn
        .query_row(
            "SELECT url FROM podcast_channels WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None);
    let Some(url) = url else { return };
    let _ = conn.execute(
        "UPDATE podcast_channels SET status = 'downloading', error = NULL WHERE id = ?1",
        [id],
    );
    match fetcher
        .fetch_feed(&url)
        .and_then(|feed| store_feed(&conn, id, &feed))
    {
        Ok(n) => {
            tracing::info!("podcast {url}: {n} new episode(s)");
            let _ = conn.execute(
                "UPDATE podcast_channels SET status = 'completed', refreshed_at = ?2 WHERE id = ?1",
                params![id, now()],
            );
        }
        Err(e) => {
            tracing::warn!("podcast {url}: {e:#}");
            let _ = conn.execute(
                "UPDATE podcast_channels SET status = 'error', error = ?2 WHERE id = ?1",
                params![id, format!("{e:#}")],
            );
        }
    }
}

fn store_feed(conn: &Connection, channel_id: &str, feed: &feed_rs::model::Feed) -> Result<usize> {
    conn.execute(
        "UPDATE podcast_channels SET title = COALESCE(?2, title), description = COALESCE(?3, description),
             image_url = COALESCE(?4, image_url) WHERE id = ?1",
        params![
            channel_id,
            text(&feed.title),
            text(&feed.description),
            feed.logo.as_ref().map(|l| l.uri.clone()).or_else(|| feed.icon.as_ref().map(|l| l.uri.clone())),
        ],
    )?;
    let mut added = 0;
    for entry in &feed.entries {
        let content = entry
            .media
            .iter()
            .flat_map(|m| m.content.iter())
            .find(|c| c.url.is_some());
        let Some(content) = content else { continue };
        let url = content
            .url
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let title = text(&entry.title).unwrap_or_else(|| "Untitled".into());
        let published = entry.published.or(entry.updated).map(|d| d.timestamp());
        let duration = content.duration.map(|d| d.as_secs() as i64).or_else(|| {
            entry
                .media
                .iter()
                .find_map(|m| m.duration)
                .map(|d| d.as_secs() as i64)
        });
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO podcast_episodes(id, channel_id, guid, title, description, published_at,
                duration_s, enclosure_url, content_type, size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                ids::random(),
                channel_id,
                entry.id,
                title,
                text(&entry.summary),
                published,
                duration,
                url,
                content.content_type.as_ref().map(ToString::to_string),
                content.size.map(|s| s as i64),
            ],
        )?;
        added += inserted;
    }
    Ok(added)
}

fn extension_for(url: &str, content_type: Option<&str>) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_lowercase();
    for ext in ["mp3", "m4a", "aac", "ogg", "opus", "flac", "wav", "mp4"] {
        if path.ends_with(&format!(".{ext}")) {
            return ext;
        }
    }
    match content_type {
        Some("audio/mp4") | Some("audio/x-m4a") => "m4a",
        Some("audio/ogg") => "ogg",
        Some("audio/opus") => "opus",
        Some("audio/flac") => "flac",
        Some("audio/aac") => "aac",
        _ => "mp3",
    }
}

pub fn download_episode(db: &Db, fetcher: &Fetcher, dir: &Path, id: &str) {
    let Ok(conn) = db.conn() else { return };
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT channel_id, enclosure_url, content_type FROM podcast_episodes WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .unwrap_or(None);
    let Some((channel, url, ctype)) = row else {
        return;
    };
    let _ = conn.execute(
        "UPDATE podcast_episodes SET status = 'downloading', error = NULL WHERE id = ?1",
        [id],
    );
    let dest: PathBuf = dir
        .join(&channel)
        .join(format!("{id}.{}", extension_for(&url, ctype.as_deref())));
    match fetcher.download(&url, &dest) {
        Ok((size, ctype)) => {
            let _ = conn.execute(
                "UPDATE podcast_episodes SET status = 'completed', path = ?2, size = ?3,
                     content_type = COALESCE(?4, content_type) WHERE id = ?1",
                params![id, dest.to_string_lossy(), size as i64, ctype],
            );
        }
        Err(e) => {
            tracing::warn!("podcast episode {url}: {e:#}");
            let _ = conn.execute(
                "UPDATE podcast_episodes SET status = 'error', error = ?2 WHERE id = ?1",
                params![id, format!("{e:#}")],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ranges_are_rejected() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.1",
            "169.254.1.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "100.64.0.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "198.18.0.1",
            "240.0.0.1",
            "192.0.0.8",
            "64:ff9b::7f00:1",
            "64:ff9b::a00:1",
            "2002:7f00:1::",
            "2001:db8::1",
            "::ffff:10.0.0.1",
            "::10.0.0.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "2606:4700:4700::1111",
            "64:ff9b::808:808",
        ] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn extension_detection() {
        assert_eq!(extension_for("https://x/y/ep1.M4A?token=1", None), "m4a");
        assert_eq!(
            extension_for("https://x/download", Some("audio/ogg")),
            "ogg"
        );
        assert_eq!(extension_for("https://x/download", None), "mp3");
    }

    #[test]
    fn relative_redirects() {
        let base: ureq::http::Uri = "http://h:81/a/b/c.xml".parse().unwrap();
        assert_eq!(resolve(&base, "/z").unwrap(), "http://h:81/z");
        assert_eq!(resolve(&base, "d.xml").unwrap(), "http://h:81/a/b/d.xml");
        assert_eq!(resolve(&base, "https://o/p").unwrap(), "https://o/p");
    }
}
