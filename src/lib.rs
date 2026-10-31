pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod ids;
pub mod jukebox;
pub mod lastfm;
pub mod live;
pub mod lyrics;
pub mod omm;
pub mod params;
pub mod podcast;
pub mod ratelimit;
pub mod response;
pub mod scanner;
pub mod state;
pub mod transcode;
pub mod upload;
pub mod util;

#[path = "../web/mod.rs"]
pub mod web;

use std::sync::Arc;

use anyhow::{Context, Result};

use crate::auth::Crypto;
use crate::config::Config;
use crate::db::Db;
use crate::scanner::Scanner;
use crate::state::AppState;

pub struct Server {
    pub state: AppState,
    _watcher: Option<scanner::watch::Supervisor>,
}

impl Server {
    pub fn router(&self) -> axum::Router {
        api::router(self.state.clone())
    }
}

fn bootstrap_admin(cfg: &Config, db: &Db, crypto: &Crypto) -> Result<()> {
    let conn = db.conn()?;
    if db::users::count(&conn)? > 0 {
        return Ok(());
    }
    let name = cfg.admin.user.clone().unwrap_or_else(|| "admin".into());
    let (password, generated) = match &cfg.admin.password {
        Some(p) => (p.clone(), false),
        None => (ids::random(), true),
    };
    if password == "change-me" {
        tracing::warn!("JUUZ_ADMIN_PASSWORD is the example value 'change-me', change it");
    }
    let hash = auth::hash_password(&password)?;
    let secret = crypto.seal(password.as_bytes());
    db::users::create(&conn, &name, &hash, Some(&secret), true)?;
    if generated {
        tracing::warn!(
            "created admin user '{name}' with generated password '{password}'; change it"
        );
    } else {
        tracing::info!("created admin user '{name}'");
    }
    Ok(())
}

pub fn init(mut cfg: Config) -> Result<Server> {
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("creating data dir {}", cfg.data_dir.display()))?;
    cfg.music_dir = std::fs::canonicalize(&cfg.music_dir)
        .with_context(|| format!("music dir {} not accessible", cfg.music_dir.display()))?;
    let db = Db::open(&cfg.db_path(), cfg.music_dir.clone())?;
    tracing::info!("music dir {}", cfg.music_dir.display());

    let crypto = Arc::new(Crypto::load_or_create(&cfg.secret_path())?);
    bootstrap_admin(&cfg, &db, &crypto)?;

    let conn = db.conn()?;
    for library in db::libraries::list(&conn)? {
        clean_stale_uploads(&library.path);
    }
    drop(conn);

    let live = live::Live::new();
    let scanner = Scanner::new(db.clone(), live.clone());
    if cfg.scan.on_start {
        scanner.start(false);
    }

    let watcher = match scanner::watch::spawn(
        scanner.clone(),
        db.clone(),
        cfg.scan.watch,
        cfg.art_cache_dir(),
    ) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!("library supervisor failed to start: {e:#}");
            None
        }
    };

    let (omm_client, omm_handle) = if cfg.omm.enabled {
        let client = Arc::new(omm::Client::new(&cfg.omm));
        let handle = omm::spawn(db.clone(), client.clone(), cfg.omm_art_dir());
        tracing::info!("omm enrichment enabled ({})", cfg.omm.base_url);
        (Some(client), Some(handle))
    } else {
        (None, None)
    };

    let cfg_ratelimit = cfg.ratelimit.clone();
    let cfg_podcasts = cfg.podcasts.clone();
    let cfg_lrclib = cfg.lrclib_url.clone();
    let cfg_lastfm = lastfm::Client::new(&cfg.lastfm).map(Arc::new);
    if cfg_lastfm.is_some() {
        tracing::info!("last.fm scrobbling enabled");
    }
    let cfg_omm_upload = cfg
        .omm
        .enabled
        .then(|| Arc::new(omm::Client::for_uploads(&cfg.omm)));
    let jukebox = jukebox::Jukebox::new(
        db.clone(),
        crypto.clone(),
        cfg.listen
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(4533),
        live.clone(),
    );
    jukebox.start();
    let state = AppState {
        db,
        cfg: Arc::new(cfg),
        crypto,
        scanner,
        lyrics: Arc::new(lyrics::LrclibClient::new(&cfg_lrclib)),
        lastfm: cfg_lastfm,
        omm_upload: cfg_omm_upload,
        omm: omm_client,
        omm_handle,
        podcasts: Arc::new(podcast::Fetcher::new(&cfg_podcasts)),
        transcode_slots: Arc::new(tokio::sync::Semaphore::new(
            std::thread::available_parallelism()
                .map(|n| (n.get() / 2).max(1))
                .unwrap_or(1),
        )),
        upload_lock: Default::default(),
        ratelimit: Arc::new(ratelimit::RateLimiter::new(&cfg_ratelimit)),
        now_playing: Default::default(),
        live,
        jukebox,
    };
    jukebox::stats::spawn(state.clone());
    Ok(Server {
        state,
        _watcher: watcher,
    })
}

fn clean_stale_uploads(root: &std::path::Path) {
    let dir = root.join(".juuz-tmp");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in entries.flatten() {
        let _ = std::fs::remove_file(e.path());
    }
    let _ = std::fs::remove_dir(&dir);
}
