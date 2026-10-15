pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod ids;
pub mod lastfm;
pub mod lyrics;
pub mod omm;
pub mod params;
pub mod podcast;
pub mod response;
pub mod scanner;
pub mod state;
pub mod transcode;
pub mod upload;
pub mod util;

use std::sync::Arc;

use anyhow::{Context, Result};

use crate::auth::Crypto;
use crate::config::Config;
use crate::db::Db;
use crate::scanner::Scanner;
use crate::state::AppState;

pub struct Server {
    pub state: AppState,
    _watcher: Option<scanner::watch::Watcher>,
}

impl Server {
    pub fn router(&self) -> axum::Router {
        api::router(self.state.clone())
    }
}

fn sync_folders(cfg: &Config, db: &Db) -> Result<()> {
    let conn = db.conn()?;
    for path in &cfg.music_folders {
        let abs = std::fs::canonicalize(path)
            .with_context(|| format!("music folder {} not accessible", path.display()))?;
        let name = abs
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Music".into());
        conn.execute(
            "INSERT OR IGNORE INTO music_folders(path, name) VALUES (?1, ?2)",
            rusqlite::params![abs.to_string_lossy(), name],
        )?;
        tracing::info!("music folder {}", abs.display());
    }
    Ok(())
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

pub fn init(cfg: Config) -> Result<Server> {
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("creating data dir {}", cfg.data_dir.display()))?;
    let db = Db::open(&cfg.db_path())?;
    sync_folders(&cfg, &db)?;

    let crypto = Arc::new(Crypto::load_or_create(&cfg.secret_path())?);
    bootstrap_admin(&cfg, &db, &crypto)?;

    clean_stale_uploads(&db);

    let scanner = Scanner::new(db.clone());
    if cfg.scan.on_start {
        scanner.start(false);
    }

    let watcher = if cfg.scan.watch {
        let roots: Vec<std::path::PathBuf> = {
            let conn = db.conn()?;
            let mut stmt = conn.prepare("SELECT path FROM music_folders")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .map(Into::into)
                .collect()
        };
        match scanner::watch::spawn(scanner.clone(), roots) {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::warn!("file watcher disabled: {e:#}");
                None
            }
        }
    } else {
        None
    };

    let (omm_client, omm_handle) = if cfg.omm.enabled {
        let client = Arc::new(omm::Client::new(&cfg.omm));
        let handle = omm::spawn(db.clone(), client.clone(), cfg.omm_art_dir());
        tracing::info!("omm enrichment enabled ({})", cfg.omm.base_url);
        (Some(client), Some(handle))
    } else {
        (None, None)
    };

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
        now_playing: Default::default(),
    };
    Ok(Server {
        state,
        _watcher: watcher,
    })
}

fn clean_stale_uploads(db: &Db) {
    let Ok(conn) = db.conn() else { return };
    let Ok(mut stmt) = conn.prepare("SELECT path FROM music_folders") else {
        return;
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return;
    };
    for root in rows.flatten() {
        let dir = std::path::Path::new(&root).join(".juuz-tmp");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
        let _ = std::fs::remove_dir(&dir);
    }
}
