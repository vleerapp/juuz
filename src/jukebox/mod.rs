pub mod dlna;
pub mod stats;

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, MutexGuard, broadcast, watch};

use crate::auth::Crypto;
use crate::db::{Db, now_ms, settings};
use crate::error::ApiError;
use crate::live::Live;
use dlna::{Device, Renderer, TrackMeta};

const OUTPUT_KEY: &str = "jukebox_output";
const QUEUE_REFILL: usize = 200;
const QUEUE_KEY: &str = "jukebox_queue";
const POLL: Duration = Duration::from_secs(1);
const SETTLE: Duration = Duration::from_secs(3);
const SEEK_SETTLE: Duration = Duration::from_secs(1);
const SEEK_TOLERANCE_SECS: u64 = 2;
const SEEK_DELAY: Duration = Duration::from_millis(300);
const HEARTBEAT_TICKS: u32 = 10;
const IDLE_TICKS: u32 = 5;
const RETRY_TICKS: u32 = 15;
const MAX_FAILURES: u32 = 3;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub current_index: usize,
    pub playing: bool,
    pub gain: f64,
    pub position: u64,
    pub queue_length: usize,
    pub revision: u64,
    pub output: Option<String>,
    pub online: bool,
    pub timestamp: i64,
}

impl Snapshot {
    fn same_state(&self, other: &Snapshot) -> bool {
        self.current_index == other.current_index
            && self.playing == other.playing
            && (self.gain - other.gain).abs() < 0.005
            && self.queue_length == other.queue_length
            && self.revision == other.revision
            && self.output == other.output
            && self.online == other.online
    }
}

#[derive(Clone, Debug)]
pub struct Outputs {
    pub current: Option<(Device, bool)>,
    pub devices: Vec<Device>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    udn: String,
    name: String,
    location: String,
}

#[derive(Serialize, Deserialize)]
struct SavedQueue {
    queue: Vec<String>,
    index: usize,
    #[serde(default)]
    owner: Option<String>,
}

#[derive(Clone, Debug)]
pub enum PlayEvent {
    Started {
        user_id: String,
        song_id: String,
    },
    Stopped {
        user_id: String,
    },
    Played {
        user_id: String,
        song_id: String,
        at: i64,
    },
}

struct Song {
    title: String,
    artist: String,
    album: String,
    content_type: String,
    suffix: String,
    duration_secs: u64,
}

struct Inner {
    queue: Vec<String>,
    index: usize,
    playing: bool,
    base_ms: u64,
    anchor: Option<Instant>,
    gain: f64,
    loaded: Option<String>,
    settled: Instant,
    revision: u64,
    saved: Option<Device>,
    renderer: Option<Renderer>,
    known: Vec<Device>,
    failures: u32,
    ticks: u32,
    last: Option<Snapshot>,
    owner: Option<String>,
    announced: Option<String>,
    counted: bool,
    duration_ms: u64,
}

impl Inner {
    fn position_ms(&self) -> u64 {
        self.base_ms + self.anchor.map_or(0, |a| a.elapsed().as_millis() as u64)
    }

    fn halt(&mut self) {
        self.base_ms = self.position_ms();
        self.anchor = None;
        self.playing = false;
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            current_index: self.index,
            playing: self.playing,
            gain: self.gain,
            position: self.position_ms() / 1000,
            queue_length: self.queue.len(),
            revision: self.revision,
            output: self.saved.as_ref().map(|d| d.name.clone()),
            online: self.renderer.is_some(),
            timestamp: now_ms(),
        }
    }
}

pub struct Jukebox {
    inner: Mutex<Inner>,
    events: broadcast::Sender<Snapshot>,
    plays: broadcast::Sender<PlayEvent>,
    saves: watch::Sender<Option<String>>,
    stop: watch::Sender<bool>,
    db: Db,
    crypto: Arc<Crypto>,
    port: u16,
    live: Live,
}

fn no_output() -> ApiError {
    ApiError::generic("no jukebox output is configured")
}

fn offline() -> ApiError {
    ApiError::generic("the jukebox output is unreachable")
}

fn locate(saved: &Device) -> Option<Renderer> {
    if let Ok(r) = Renderer::connect(&saved.location)
        && r.device.udn == saved.udn
    {
        return Some(r);
    }
    let found = dlna::discover().into_iter().find(|d| d.udn == saved.udn)?;
    Renderer::connect(&found.location).ok()
}

fn host(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v) => v.to_string(),
        IpAddr::V6(v) => format!("[{v}]"),
    }
}

impl Jukebox {
    pub fn new(db: Db, crypto: Arc<Crypto>, port: u16, live: Live) -> Arc<Jukebox> {
        Arc::new(Jukebox {
            inner: Mutex::new(Inner {
                queue: Vec::new(),
                index: 0,
                playing: false,
                base_ms: 0,
                anchor: None,
                gain: 1.0,
                loaded: None,
                settled: Instant::now(),
                revision: 0,
                saved: None,
                renderer: None,
                known: Vec::new(),
                failures: 0,
                ticks: 0,
                last: None,
                owner: None,
                announced: None,
                counted: false,
                duration_ms: 0,
            }),
            plays: broadcast::channel(64).0,
            saves: watch::channel(None).0,
            events: broadcast::channel(32).0,
            stop: watch::channel(false).0,
            db,
            crypto,
            port,
            live,
        })
    }

    pub fn start(self: &Arc<Self>) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let jb = self.clone();
        handle.spawn(async move {
            jb.restore().await;
            jb.poll().await;
        });
        let jb = self.clone();
        handle.spawn(async move { jb.save_loop().await });
    }

    pub fn shutdown(&self) {
        let _ = self.stop.send(true);
    }

    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.stop.subscribe()
    }

    pub fn plays(&self) -> broadcast::Receiver<PlayEvent> {
        self.plays.subscribe()
    }

    pub async fn set_owner(&self, user_id: &str) {
        let mut g = self.inner.lock().await;
        if g.owner.as_deref() != Some(user_id) {
            g.owner = Some(user_id.to_string());
            self.persist(&g);
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Snapshot> {
        self.events.subscribe()
    }

    async fn restore(&self) {
        self.restore_queue().await;
        let saved = self
            .db
            .run(|c| Ok(settings::get(c, OUTPUT_KEY)?))
            .await
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<Saved>(&raw).ok());
        let Some(saved) = saved else {
            let mut g = self.inner.lock().await;
            self.publish(&mut g);
            return;
        };
        let device = Device {
            udn: saved.udn,
            name: saved.name,
            location: saved.location,
        };
        let target = device.clone();
        let renderer = tokio::task::spawn_blocking(move || locate(&target))
            .await
            .ok()
            .flatten();
        let mut g = self.inner.lock().await;
        g.saved = Some(device);
        if let Some(r) = renderer {
            self.attach(&mut g, r).await;
            self.adopt(&mut g).await;
        } else {
            tracing::warn!("jukebox output is offline");
        }
        self.publish(&mut g);
        self.live.notify();
    }

    async fn restore_queue(&self) {
        let raw = self
            .db
            .run(|c| Ok(settings::get(c, QUEUE_KEY)?))
            .await
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<SavedQueue>(&raw).ok());
        let Some(saved) = raw else {
            return;
        };
        let index = saved.index;
        let owner = saved.owner.clone();
        let queue = self
            .db
            .run(move |c| {
                let mut kept = Vec::new();
                for id in saved.queue {
                    let exists: bool = c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM songs WHERE id = ?1)",
                        [&id],
                        |r| r.get(0),
                    )?;
                    if exists {
                        kept.push(id);
                    }
                }
                Ok(kept)
            })
            .await
            .unwrap_or_default();
        let mut g = self.inner.lock().await;
        g.index = index.min(queue.len().saturating_sub(1));
        g.revision = u64::from(!queue.is_empty());
        g.queue = queue;
        g.owner = owner;
    }

    async fn adopt(&self, g: &mut Inner) {
        let Some(renderer) = g.renderer.clone() else {
            return;
        };
        if g.queue.is_empty() {
            return;
        }
        let probe = tokio::task::spawn_blocking(move || {
            anyhow::Ok((renderer.transport_state()?, renderer.position()?))
        })
        .await;
        let Ok(Ok((state, position))) = probe else {
            return;
        };
        if state != "PLAYING" && state != "PAUSED_PLAYBACK" {
            return;
        }
        let Some(uri) = position.track_uri else {
            return;
        };
        let Some(index) = g
            .queue
            .iter()
            .position(|id| uri.contains(&format!("/jukebox/stream/{id}/")))
        else {
            return;
        };
        tracing::info!("jukebox resumed track {index} of {}", g.queue.len());
        g.index = index;
        g.loaded = Some(g.queue[index].clone());
        g.base_ms = position.seconds.unwrap_or(0) * 1000;
        if state == "PLAYING" {
            g.playing = true;
            g.anchor = Some(Instant::now());
            g.settled = Instant::now();
        }
    }

    async fn attach(&self, g: &mut Inner, renderer: Renderer) {
        let probe = renderer.clone();
        if let Ok(Ok(v)) = tokio::task::spawn_blocking(move || probe.volume()).await {
            g.gain = f64::from(v) / 100.0;
        }
        g.saved = Some(renderer.device.clone());
        if !g.known.iter().any(|d| d.udn == renderer.device.udn) {
            g.known.push(renderer.device.clone());
        }
        g.renderer = Some(renderer);
        g.failures = 0;
    }

    fn publish(&self, g: &mut Inner) -> Snapshot {
        let snap = g.snapshot();
        let moved = g
            .last
            .as_ref()
            .is_none_or(|l| l.revision != snap.revision || l.current_index != snap.current_index);
        if moved {
            self.persist(g);
        }
        self.announce(g);
        g.last = Some(snap.clone());
        let _ = self.events.send(snap.clone());
        snap
    }

    fn announce(&self, g: &mut Inner) {
        let want = if g.playing { g.loaded.clone() } else { None };
        if want == g.announced {
            return;
        }
        if let Some(user_id) = g.owner.clone() {
            let event = match &want {
                Some(song_id) => PlayEvent::Started {
                    user_id,
                    song_id: song_id.clone(),
                },
                None => PlayEvent::Stopped { user_id },
            };
            let _ = self.plays.send(event);
        }
        g.announced = want;
    }

    fn finish(&self, g: &mut Inner, completed: bool) {
        let Some(song_id) = g.loaded.clone() else {
            return;
        };
        if g.counted {
            return;
        }
        let threshold = (g.duration_ms / 2).min(240_000);
        let listened = g.duration_ms >= 30_000 && g.position_ms() >= threshold;
        if !(completed || listened) {
            return;
        }
        g.counted = true;
        if let Some(user_id) = g.owner.clone() {
            let _ = self.plays.send(PlayEvent::Played {
                user_id,
                song_id,
                at: crate::db::now(),
            });
        }
    }

    fn persist(&self, g: &Inner) {
        if let Ok(raw) = serde_json::to_string(&SavedQueue {
            queue: g.queue.clone(),
            index: g.index,
            owner: g.owner.clone(),
        }) {
            self.saves.send_replace(Some(raw));
        }
    }

    async fn save_loop(&self) {
        let mut saves = self.saves.subscribe();
        let mut stop = self.stop.subscribe();
        loop {
            tokio::select! {
                _ = stop.changed() => return,
                changed = saves.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let raw = saves.borrow_and_update().clone();
                    if let Some(raw) = raw {
                        let _ = self.db.run(move |c| Ok(settings::set(c, QUEUE_KEY, &raw)?)).await;
                    }
                }
            }
        }
    }

    pub async fn snapshot(&self) -> Snapshot {
        self.inner.lock().await.snapshot()
    }

    pub async fn state(&self) -> (Snapshot, Vec<String>) {
        let g = self.inner.lock().await;
        (g.snapshot(), g.queue.clone())
    }

    pub fn sign(&self, id: &str) -> String {
        self.crypto.sign(&format!("jukebox:{id}"))
    }

    pub fn verify(&self, id: &str, sig: &str) -> bool {
        crate::auth::constant_eq(&self.sign(id), sig)
    }

    async fn song(&self, id: &str) -> Result<Song, ApiError> {
        let id = id.to_string();
        self.db
            .run(move |c| {
                use rusqlite::OptionalExtension;
                c.query_row(
                    "SELECT s.title, s.display_artist, al.title, s.content_type, s.suffix, s.duration_ms
                     FROM songs s JOIN albums al ON al.id = s.album_id WHERE s.id = ?1",
                    [&id],
                    |r| {
                        Ok(Song {
                            title: r.get(0)?,
                            artist: r.get(1)?,
                            album: r.get(2)?,
                            content_type: r.get(3)?,
                            suffix: r.get(4)?,
                            duration_secs: (r.get::<_, i64>(5)?.max(0) / 1000) as u64,
                        })
                    },
                )
                .optional()?
                .ok_or_else(|| ApiError::not_found("song"))
            })
            .await
    }

    pub async fn ensure_songs(&self, ids: &[String]) -> Result<(), ApiError> {
        let ids = ids.to_vec();
        self.db
            .run(move |c| {
                for id in &ids {
                    let exists: bool = c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM songs WHERE id = ?1)",
                        [id],
                        |r| r.get(0),
                    )?;
                    if !exists {
                        return Err(ApiError::not_found("song"));
                    }
                }
                Ok(())
            })
            .await
    }

    async fn with_renderer<T, F>(&self, g: &mut Inner, f: F) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: Fn(&Renderer) -> anyhow::Result<T> + Clone + Send + 'static,
    {
        let Some(renderer) = g.renderer.clone() else {
            return Err(if g.saved.is_some() {
                offline()
            } else {
                no_output()
            });
        };
        let first = f.clone();
        match tokio::task::spawn_blocking(move || first(&renderer)).await {
            Ok(Ok(v)) => return Ok(v),
            Ok(Err(e)) if e.downcast_ref::<ureq::Error>().is_some() => {
                tracing::warn!("jukebox output unreachable: {e:#}");
            }
            Ok(Err(e)) => {
                return Err(ApiError::generic(format!("jukebox output failed: {e:#}")));
            }
            Err(_) => return Err(ApiError::generic("internal server error")),
        }
        let saved = g.saved.clone().ok_or_else(no_output)?;
        let fresh = tokio::task::spawn_blocking(move || locate(&saved))
            .await
            .ok()
            .flatten();
        let Some(fresh) = fresh else {
            g.renderer = None;
            return Err(offline());
        };
        g.renderer = Some(fresh.clone());
        match tokio::task::spawn_blocking(move || f(&fresh)).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(ApiError::generic(format!("jukebox output failed: {e:#}"))),
            Err(_) => Err(ApiError::generic("internal server error")),
        }
    }

    async fn load(&self, g: &mut Inner, index: usize, offset: u64) -> Result<(), ApiError> {
        let id = g.queue[index].clone();
        tracing::info!(
            "jukebox loading {id} (queue {index}/{}) at {offset}s",
            g.queue.len()
        );
        self.finish(g, false);
        let result = self.load_id(g, &id, offset).await;
        match result {
            Ok(duration_ms) => {
                g.duration_ms = duration_ms;
                g.counted = false;
                g.loaded = Some(id);
                g.base_ms = offset * 1000;
                g.anchor = Some(Instant::now());
                g.settled = Instant::now();
                g.playing = true;
                Ok(())
            }
            Err(e) => {
                g.halt();
                Err(e)
            }
        }
    }

    async fn load_id(&self, g: &mut Inner, id: &str, offset: u64) -> Result<u64, ApiError> {
        let song = self.song(id).await?;
        let duration_ms = song.duration_secs * 1000;
        let path = format!("/jukebox/stream/{id}/{}.{}", self.sign(id), song.suffix);
        let port = self.port;
        self.with_renderer(g, move |r| {
            let url = format!("http://{}:{port}{path}", host(r.local_ip()?));
            let meta = dlna::didl(&TrackMeta {
                title: &song.title,
                artist: &song.artist,
                album: &song.album,
                content_type: &song.content_type,
                url: &url,
                duration_secs: song.duration_secs,
            });
            r.set_uri(&url, &meta)?;
            r.play()?;
            if offset > 0 {
                std::thread::sleep(SEEK_DELAY);
                if let Err(e) = r.seek(offset) {
                    tracing::debug!("seek after load failed: {e:#}");
                }
            }
            Ok(())
        })
        .await?;
        Ok(duration_ms)
    }

    pub async fn start_playback(&self) -> Result<Snapshot, ApiError> {
        let mut g = self.inner.lock().await;
        if g.queue.is_empty() {
            return Ok(self.publish(&mut g));
        }
        if g.renderer.is_none() {
            return Err(if g.saved.is_some() {
                offline()
            } else {
                no_output()
            });
        }
        let index = g.index.min(g.queue.len() - 1);
        g.index = index;
        let same = g.loaded.as_deref() == Some(g.queue[index].as_str());
        let resumed = same
            && self
                .with_renderer(&mut g, |r| match r.transport_state()?.as_str() {
                    "PLAYING" | "TRANSITIONING" => Ok(()),
                    _ => r.play(),
                })
                .await
                .is_ok();
        if resumed {
            if !g.playing {
                g.anchor = Some(Instant::now());
                g.settled = Instant::now();
                g.playing = true;
            }
        } else {
            let offset = g.base_ms / 1000;
            if let Err(e) = self.load(&mut g, index, offset).await {
                self.publish(&mut g);
                return Err(e);
            }
        }
        Ok(self.publish(&mut g))
    }

    pub async fn stop_playback(&self) -> Result<Snapshot, ApiError> {
        let mut g = self.inner.lock().await;
        if g.playing {
            g.halt();
            if let Err(e) = self
                .with_renderer(&mut g, |r| r.pause().or_else(|_| r.stop()))
                .await
            {
                tracing::warn!("pausing jukebox output: {e}");
            }
        }
        Ok(self.publish(&mut g))
    }

    pub async fn skip(&self, index: usize, offset: u64) -> Result<Snapshot, ApiError> {
        let mut g = self.inner.lock().await;
        if index >= g.queue.len() {
            return Err(ApiError::bad_param("index"));
        }
        g.index = index;
        if g.playing {
            let same = g.loaded.as_deref() == Some(g.queue[index].as_str());
            let close = same && offset.abs_diff(g.position_ms() / 1000) <= SEEK_TOLERANCE_SECS;
            let sought = !close
                && same
                && self
                    .with_renderer(&mut g, move |r| r.seek(offset))
                    .await
                    .is_ok();
            if close {
            } else if sought {
                g.base_ms = offset * 1000;
                g.anchor = Some(Instant::now());
                g.settled = Instant::now()
                    .checked_sub(SETTLE - SEEK_SETTLE)
                    .unwrap_or_else(Instant::now);
            } else if let Err(e) = self.load(&mut g, index, offset).await {
                self.publish(&mut g);
                return Err(e);
            }
        } else {
            g.loaded = None;
            g.base_ms = offset * 1000;
            g.anchor = None;
        }
        Ok(self.publish(&mut g))
    }

    pub async fn set(&self, ids: Vec<String>) -> Result<Snapshot, ApiError> {
        self.ensure_songs(&ids).await?;
        let mut g = self.inner.lock().await;
        g.queue = ids;
        g.index = 0;
        g.base_ms = 0;
        g.anchor = None;
        g.revision += 1;
        if g.queue.is_empty() {
            self.silence(&mut g).await;
        } else if g.playing {
            g.anchor = Some(Instant::now());
        }
        Ok(self.publish(&mut g))
    }

    pub async fn add(&self, ids: Vec<String>) -> Result<Snapshot, ApiError> {
        self.ensure_songs(&ids).await?;
        let mut g = self.inner.lock().await;
        g.queue.extend(ids);
        g.revision += 1;
        Ok(self.publish(&mut g))
    }

    pub async fn clear(&self) -> Result<Snapshot, ApiError> {
        let mut g = self.inner.lock().await;
        g.queue.clear();
        g.index = 0;
        g.revision += 1;
        self.silence(&mut g).await;
        Ok(self.publish(&mut g))
    }

    async fn silence(&self, g: &mut Inner) {
        self.finish(g, false);
        let was_loaded = g.loaded.take().is_some();
        g.playing = false;
        g.base_ms = 0;
        g.anchor = None;
        if was_loaded && let Err(e) = self.with_renderer(g, |r| r.stop()).await {
            tracing::warn!("stopping jukebox output: {e}");
        }
    }

    pub async fn remove(&self, index: usize) -> Result<Snapshot, ApiError> {
        let mut g = self.inner.lock().await;
        if index >= g.queue.len() {
            return Err(ApiError::bad_param("index"));
        }
        g.queue.remove(index);
        g.revision += 1;
        if index < g.index {
            g.index -= 1;
        } else if index == g.index {
            g.base_ms = 0;
            g.anchor = None;
            if g.queue.is_empty() {
                g.index = 0;
                self.silence(&mut g).await;
            } else {
                g.index = g.index.min(g.queue.len() - 1);
                let current = g.index;
                if g.playing
                    && let Err(e) = self.load(&mut g, current, 0).await
                {
                    self.publish(&mut g);
                    return Err(e);
                }
            }
        }
        Ok(self.publish(&mut g))
    }

    pub async fn shuffle(&self) -> Result<Snapshot, ApiError> {
        let mut g = self.inner.lock().await;
        let current = g.queue.get(g.index).cloned();
        g.queue.shuffle(&mut rand::rng());
        if let Some(id) = current {
            g.index = g.queue.iter().position(|q| *q == id).unwrap_or(0);
        }
        g.revision += 1;
        Ok(self.publish(&mut g))
    }

    pub async fn set_gain(&self, gain: f64) -> Result<Snapshot, ApiError> {
        let gain = gain.clamp(0.0, 1.0);
        let mut g = self.inner.lock().await;
        g.gain = gain;
        if g.renderer.is_some() {
            let percent = (gain * 100.0).round() as u32;
            if let Err(e) = self
                .with_renderer(&mut g, move |r| r.set_volume(percent))
                .await
            {
                tracing::warn!("setting jukebox volume: {e}");
            }
        }
        Ok(self.publish(&mut g))
    }

    pub async fn outputs(&self) -> Outputs {
        let g = self.inner.lock().await;
        Outputs {
            current: g.saved.clone().map(|d| (d, g.renderer.is_some())),
            devices: g.known.clone(),
        }
    }

    pub async fn scan(&self) -> Result<Vec<Device>, ApiError> {
        let found = tokio::task::spawn_blocking(dlna::discover)
            .await
            .map_err(|_| ApiError::generic("internal server error"))?;
        let mut g = self.inner.lock().await;
        for d in found {
            match g.known.iter_mut().find(|k| k.udn == d.udn) {
                Some(k) => *k = d,
                None => g.known.push(d),
            }
        }
        self.live.notify();
        Ok(g.known.clone())
    }

    pub async fn select(&self, udn: Option<&str>, url: Option<&str>) -> Result<(), ApiError> {
        let renderer = match (url, udn) {
            (Some(url), _) => {
                let url = url.to_string();
                Some(
                    tokio::task::spawn_blocking(move || Renderer::connect(&url))
                        .await
                        .map_err(|_| ApiError::generic("internal server error"))?
                        .map_err(|e| ApiError::generic(format!("cannot use renderer: {e:#}")))?,
                )
            }
            (None, Some(udn)) => {
                let device = self
                    .inner
                    .lock()
                    .await
                    .known
                    .iter()
                    .find(|d| d.udn == udn)
                    .cloned()
                    .ok_or_else(|| ApiError::not_found("renderer"))?;
                Some(
                    tokio::task::spawn_blocking(move || Renderer::connect(&device.location))
                        .await
                        .map_err(|_| ApiError::generic("internal server error"))?
                        .map_err(|e| ApiError::generic(format!("cannot use renderer: {e:#}")))?,
                )
            }
            (None, None) => None,
        };

        let mut g = self.inner.lock().await;
        self.finish(&mut g, false);
        if g.renderer.is_some() && g.loaded.is_some() {
            let _ = self.with_renderer(&mut g, |r| r.stop()).await;
        }
        g.halt();
        g.loaded = None;
        g.renderer = None;
        g.saved = None;

        let db = self.db.clone();
        match renderer {
            Some(r) => {
                let saved = serde_json::to_string(&Saved {
                    udn: r.device.udn.clone(),
                    name: r.device.name.clone(),
                    location: r.device.location.clone(),
                })
                .map_err(|_| ApiError::generic("internal server error"))?;
                db.run(move |c| Ok(settings::set(c, OUTPUT_KEY, &saved)?))
                    .await?;
                self.attach(&mut g, r).await;
            }
            None => {
                db.run(|c| Ok(settings::delete(c, OUTPUT_KEY)?)).await?;
            }
        }
        self.publish(&mut g);
        self.live.notify();
        Ok(())
    }

    async fn poll(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        let mut timer = tokio::time::interval(POLL);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = stop.changed() => return,
                _ = timer.tick() => self.tick().await,
            }
        }
    }

    async fn tick(&self) {
        let mut g = self.inner.lock().await;
        g.ticks = g.ticks.wrapping_add(1);

        let Some(renderer) = g.renderer.clone() else {
            if g.saved.is_some() && g.ticks % RETRY_TICKS == 0 {
                let saved = g.saved.clone();
                drop(g);
                self.retry(saved).await;
            }
            return;
        };
        if !g.playing && g.ticks % IDLE_TICKS != 0 {
            return;
        }

        let probe = tokio::task::spawn_blocking(move || {
            let state = renderer.transport_state()?;
            let position = renderer.position().ok();
            let volume = renderer.volume().ok();
            anyhow::Ok((state, position, volume))
        })
        .await;

        let (state, position, volume) = match probe {
            Ok(Ok(v)) => v,
            _ => {
                g.failures += 1;
                if g.failures >= MAX_FAILURES {
                    tracing::warn!("jukebox output stopped responding");
                    g.renderer = None;
                    g.halt();
                    self.publish(&mut g);
                    self.live.notify();
                }
                return;
            }
        };
        g.failures = 0;

        if let Some(v) = volume {
            g.gain = f64::from(v) / 100.0;
        }
        if g.playing {
            match state.as_str() {
                "PLAYING" => {
                    if let Some(secs) = position.and_then(|p| p.seconds) {
                        g.base_ms = secs * 1000;
                        g.anchor = Some(Instant::now());
                    }
                }
                "PAUSED_PLAYBACK" if g.settled.elapsed() > SETTLE => {
                    if let Some(secs) = position.and_then(|p| p.seconds) {
                        g.base_ms = secs * 1000;
                    }
                    g.anchor = None;
                    g.playing = false;
                }
                "STOPPED" | "NO_MEDIA_PRESENT" if g.settled.elapsed() > SETTLE => {
                    self.advance(&mut g).await;
                }
                _ => {}
            }
        }

        let snap = g.snapshot();
        let changed = g.last.as_ref().is_none_or(|l| !l.same_state(&snap));
        if changed || (g.playing && g.ticks % HEARTBEAT_TICKS == 0) {
            self.publish(&mut g);
        }
    }

    async fn advance(&self, g: &mut MutexGuard<'_, Inner>) {
        self.finish(g, true);
        let replaced = g.loaded.as_deref() != g.queue.get(g.index).map(String::as_str);
        let next = if replaced { g.index } else { g.index + 1 };
        if next >= g.queue.len() {
            self.extend_from_play_queue(g).await;
        }
        tracing::info!(
            "jukebox track ended, queue {}/{}",
            next.min(g.queue.len()),
            g.queue.len()
        );
        if next < g.queue.len() {
            g.index = next;
            if let Err(e) = self.load(g, next, 0).await {
                tracing::warn!("advancing jukebox queue: {e}");
            }
        } else {
            g.loaded = None;
            g.base_ms = 0;
            g.anchor = None;
            g.playing = false;
        }
    }

    async fn extend_from_play_queue(&self, g: &mut Inner) {
        let (Some(owner), Some(last)) = (g.owner.clone(), g.loaded.clone()) else {
            return;
        };
        let rest = self
            .db
            .run(move |c| {
                use rusqlite::OptionalExtension;
                let current: Option<i64> = c
                    .query_row(
                        "SELECT current_index FROM play_queue WHERE user_id = ?1",
                        [&owner],
                        |r| r.get(0),
                    )
                    .optional()?
                    .flatten();
                let mut stmt = c.prepare(
                    "SELECT song_id FROM play_queue_songs WHERE user_id = ?1 ORDER BY position",
                )?;
                let ids: Vec<String> = stmt
                    .query_map([&owner], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                let from = current.unwrap_or(0).max(0) as usize;
                let at = ids
                    .iter()
                    .skip(from)
                    .position(|id| *id == last)
                    .map(|i| i + from)
                    .or_else(|| ids.iter().position(|id| *id == last));
                Ok(at
                    .map(|i| ids[i + 1..].iter().take(QUEUE_REFILL).cloned().collect())
                    .unwrap_or_else(Vec::new))
            })
            .await
            .unwrap_or_default();
        if !rest.is_empty() {
            tracing::info!(
                "jukebox continuing with {} songs from the saved play queue",
                rest.len()
            );
            g.queue.extend(rest);
            g.revision += 1;
        }
    }

    async fn retry(&self, saved: Option<Device>) {
        let Some(saved) = saved else {
            return;
        };
        let target = saved.clone();
        let found = tokio::task::spawn_blocking(move || locate(&target))
            .await
            .ok()
            .flatten();
        let Some(renderer) = found else {
            return;
        };
        let mut g = self.inner.lock().await;
        if g.renderer.is_none() && g.saved.as_ref().is_some_and(|s| s.udn == saved.udn) {
            tracing::info!("jukebox output is back");
            self.attach(&mut g, renderer).await;
            self.publish(&mut g);
            self.live.notify();
        }
    }
}
