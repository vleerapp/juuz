use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::auth::Crypto;
use crate::config::Config;
use crate::db::Db;
use crate::live::Live;
use crate::omm;
use crate::scanner::Scanner;

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
    pub lyrics: Arc<crate::lyrics::LrclibClient>,
    pub lastfm: Option<Arc<crate::lastfm::Client>>,
    pub omm_upload: Option<Arc<omm::Client>>,
    pub omm_handle: Option<omm::Handle>,
    pub podcasts: Arc<crate::podcast::Fetcher>,
    pub transcode_slots: Arc<tokio::sync::Semaphore>,
    pub upload_lock: Arc<tokio::sync::Mutex<()>>,
    pub ratelimit: Arc<crate::ratelimit::RateLimiter>,
    pub now_playing: Arc<Mutex<HashMap<(String, String), NowPlaying>>>,
    pub live: Live,
    pub jukebox: Arc<crate::jukebox::Jukebox>,
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
    let now = crate::db::now();
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
        let now = crate::db::now();
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
