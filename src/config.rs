use std::path::PathBuf;

use anyhow::{Result, bail};

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: String,
    pub data_dir: PathBuf,
    pub music_dir: PathBuf,
    pub scan: ScanConfig,
    pub upload: UploadConfig,
    pub omm: OmmConfig,
    pub admin: AdminConfig,
    pub podcasts: PodcastConfig,
    pub lastfm: LastfmConfig,
    pub lrclib_url: String,
    pub public_url: Option<String>,
    pub ratelimit: RateLimitConfig,
}

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub per_minute: u32,
    pub burst: u32,
    pub trust_proxy: bool,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            per_minute: 1800,
            burst: 300,
            trust_proxy: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub on_start: bool,
    pub watch: bool,
}

#[derive(Debug, Clone)]
pub struct UploadConfig {
    pub enabled: bool,
    pub max_size: u64,
    pub max_companion_size: u64,
}

#[derive(Debug, Clone)]
pub struct OmmConfig {
    pub enabled: bool,
    pub base_url: String,
    pub timeout_secs: u64,
    pub upload_timeout_secs: u64,
}

#[derive(Debug, Clone, Default)]
pub struct AdminConfig {
    pub user: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LastfmConfig {
    pub api_key: Option<String>,
    pub secret: Option<String>,
    pub api_url: String,
    pub auth_url: String,
}

impl Default for LastfmConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            secret: None,
            api_url: "https://ws.audioscrobbler.com/2.0/".into(),
            auth_url: "https://www.last.fm/api/auth/".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PodcastConfig {
    pub enabled: bool,
    pub allow_private_hosts: bool,
    pub max_episode_size: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:4533".into(),
            data_dir: PathBuf::from("./data"),
            music_dir: PathBuf::from("./music"),
            scan: ScanConfig::default(),
            upload: UploadConfig::default(),
            omm: OmmConfig::default(),
            admin: AdminConfig::default(),
            podcasts: PodcastConfig::default(),
            lastfm: LastfmConfig::default(),
            lrclib_url: "https://lrclib.net/api".into(),
            public_url: None,
            ratelimit: RateLimitConfig::default(),
        }
    }
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            on_start: true,
            watch: true,
        }
    }
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_size: 2 * 1024 * 1024 * 1024,
            max_companion_size: 10 * 1024 * 1024,
        }
    }
}

impl Default for OmmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            base_url: "https://api.vleer.app/metadata/v1".into(),
            timeout_secs: 20,
            upload_timeout_secs: 5,
        }
    }
}

impl Default for PodcastConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_private_hosts: false,
            max_episode_size: 2 * 1024 * 1024 * 1024,
        }
    }
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn flag(name: &str, default: bool) -> Result<bool> {
    match var(name) {
        None => Ok(default),
        Some(v) => match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => bail!("{name} must be true or false, got {v:?}"),
        },
    }
}

impl Config {
    pub fn from_env() -> Result<Config> {
        let mut cfg = Config::default();
        cfg.admin.password = var("JUUZ_ADMIN_PASSWORD");
        cfg.public_url = var("JUUZ_PUBLIC_URL").map(|u| u.trim().trim_end_matches('/').to_string());
        cfg.ratelimit.trust_proxy = flag("JUUZ_TRUST_PROXY", cfg.ratelimit.trust_proxy)?;
        if let Some(v) = var("JUUZ_RATE_LIMIT") {
            cfg.ratelimit.per_minute = v
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("JUUZ_RATE_LIMIT must be a number, got {v:?}"))?;
        }
        cfg.omm.enabled = flag("JUUZ_OMM", cfg.omm.enabled)?;
        cfg.lastfm.api_key = var("JUUZ_LASTFM_API_KEY");
        cfg.lastfm.secret = var("JUUZ_LASTFM_SECRET");
        if cfg.lastfm.api_key.is_some() != cfg.lastfm.secret.is_some() {
            tracing::warn!(
                "JUUZ_LASTFM_API_KEY and JUUZ_LASTFM_SECRET must both be set, last.fm is disabled"
            );
        }
        Ok(cfg)
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("juuz.db")
    }

    pub fn art_cache_dir(&self) -> PathBuf {
        self.data_dir.join("cache").join("art")
    }

    pub fn omm_art_dir(&self) -> PathBuf {
        self.data_dir.join("cache").join("omm-art")
    }

    pub fn transcode_dir(&self) -> PathBuf {
        self.data_dir.join("cache").join("transcode")
    }

    pub fn podcast_dir(&self) -> PathBuf {
        self.data_dir.join("podcasts")
    }

    pub fn secret_path(&self) -> PathBuf {
        self.data_dir.join("secret.key")
    }
}
