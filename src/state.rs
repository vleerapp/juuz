use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::auth::Crypto;
use crate::config::Config;
use crate::db::Db;
use crate::omm;
use crate::scanner::Scanner;

#[derive(Clone, Debug)]
pub struct NowPlaying {
    pub song_id: String,
    pub username: String,
    pub player: String,
    pub started: i64,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub cfg: Arc<Config>,
    pub crypto: Arc<Crypto>,
    pub scanner: Scanner,
    pub omm: Option<Arc<omm::Client>>,
    pub lyrics: Arc<crate::lyrics::LrclibClient>,
    pub lastfm: Option<Arc<crate::lastfm::Client>>,
    pub omm_upload: Option<Arc<omm::Client>>,
    pub omm_handle: Option<omm::Handle>,
    pub podcasts: Arc<crate::podcast::Fetcher>,
    pub transcode_slots: Arc<tokio::sync::Semaphore>,
    pub upload_lock: Arc<tokio::sync::Mutex<()>>,
    pub now_playing: Arc<Mutex<HashMap<String, NowPlaying>>>,
}
