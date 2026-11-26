use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::server::api;
use crate::server::middleware::rate_limit;
use crate::server::utils::auth::{self, Crypto};
use crate::server::utils::config::Config;
use crate::server::utils::db::{self, Db};
use crate::server::utils::live::Live;
use crate::server::utils::omm;
use crate::server::utils::scanner::{self, Scanner};
use crate::server::utils::{ids, jukebox, lastfm, live, lyrics};

#[derive(Clone, Debug)]
pub struct NowPlaying {
    pub user_id: String,
    pub song_id: String,
    pub username: String,
    pub player: String,
    pub started: i64,
    pub until: i64,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub cfg: Arc<Config>,
    pub crypto: Arc<Crypto>,
    pub scanner: Scanner,
    pub omm: Option<Arc<omm::Client>>,
    pub lyrics: Arc<crate::server::utils::lyrics::LrclibClient>,
    pub lastfm: Option<Arc<crate::server::utils::lastfm::Client>>,
    pub omm_upload: Option<Arc<omm::Client>>,
    pub omm_handle: Option<omm::Handle>,
    pub transcode_slots: Arc<tokio::sync::Semaphore>,
    pub upload_lock: Arc<tokio::sync::Mutex<()>>,
    pub ratelimit: Arc<crate::server::middleware::rate_limit::RateLimiter>,
    pub now_playing: Arc<Mutex<HashMap<(String, String), NowPlaying>>>,
    pub live: Live,
    pub jukebox: Arc<crate::server::utils::jukebox::Jukebox>,
}

pub const NOW_PLAYING_TTL: i64 = 600;
pub const JUKEBOX_PLAYER: &str = "jukebox";

pub const MAX_PLAYER_NAME: usize = 64;
const MAX_NOW_PLAYING: usize = 1000;
const MAX_PLAYERS_PER_USER: usize = 8;

pub fn player_name(raw: Option<&str>) -> String {
    raw.unwrap_or("unknown")
        .chars()
        .take(MAX_PLAYER_NAME)
        .collect()
}

pub fn prune_now_playing(map: &mut HashMap<(String, String), NowPlaying>, user_id: &str) {
    let now = crate::server::utils::db::now();
    if map.len() >= MAX_NOW_PLAYING {
        map.retain(|_, n| n.until > now);
    }
    let mut mine: Vec<((String, String), i64)> = map
        .iter()
        .filter(|(_, n)| n.user_id == user_id)
        .map(|(k, n)| (k.clone(), n.started))
        .collect();
    if mine.len() >= MAX_PLAYERS_PER_USER {
        mine.sort_by_key(|(_, started)| *started);
        let excess = mine.len() + 1 - MAX_PLAYERS_PER_USER;
        for (key, _) in mine.into_iter().take(excess) {
            map.remove(&key);
        }
    }
}

impl AppState {
    pub fn playing_now(&self) -> Vec<NowPlaying> {
        let now = crate::server::utils::db::now();
        let mut map = self.now_playing.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, n| n.until > now);
        let mut all: Vec<NowPlaying> = map.values().cloned().collect();
        drop(map);
        all.sort_by(|a, b| {
            (b.player == JUKEBOX_PLAYER, b.started).cmp(&(a.player == JUKEBOX_PLAYER, a.started))
        });
        let mut seen = std::collections::HashSet::new();
        all.retain(|n| seen.insert((n.user_id.clone(), n.song_id.clone())));
        all.sort_by_key(|n| std::cmp::Reverse(n.started));
        all
    }
}

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
    let secret = crypto.token_secret(&password);
    db::users::create(&conn, &name, &hash, secret.as_deref(), true)?;
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

    let crypto =
        Arc::new(Crypto::load_or_create(&cfg.secret_path())?.with_token_auth(cfg.token_auth));
    if !cfg.token_auth {
        let cleared = db.conn()?.execute(
            "UPDATE users SET token_secret = NULL WHERE token_secret IS NOT NULL",
            [],
        )?;
        if cleared > 0 {
            tracing::info!(
                "token authentication is off, removed {cleared} stored password secrets"
            );
        }
    }
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
        let handle = omm::spawn(db.clone(), client.clone(), cfg.omm_art_dir(), live.clone());
        tracing::info!("omm enrichment enabled ({})", cfg.omm.base_url);
        (Some(client), Some(handle))
    } else {
        (None, None)
    };

    let cfg_ratelimit = cfg.ratelimit.clone();
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
        cfg.snapcast.then_some(cfg.snapcast_port),
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
        transcode_slots: Arc::new(tokio::sync::Semaphore::new(
            std::thread::available_parallelism()
                .map(|n| (n.get() / 2).max(1))
                .unwrap_or(1),
        )),
        upload_lock: Default::default(),
        ratelimit: Arc::new(rate_limit::RateLimiter::new(&cfg_ratelimit)),
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
