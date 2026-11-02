pub mod airplay;
pub mod browser;
pub mod dlna;
pub mod group;
pub mod output;
pub mod snapcast;
pub mod stats;
pub mod stream;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, MutexGuard, broadcast, watch};

use crate::auth::Crypto;
use crate::db::{Db, now_ms, settings};
use crate::error::ApiError;
use crate::live::Live;
use dlna::Device;
use group::Group;
use output::{Directory, Output, SharedOutput, State, Track};

const OUTPUT_KEY: &str = "jukebox_output";
const GROUPS_KEY: &str = "jukebox_groups";
const GAIN_KEY: &str = "jukebox_gain";
const AIRPLAY_AUTH_KEY: &str = "jukebox_airplay_auth";
const PAIRING_WINDOW: Duration = Duration::from_secs(120);
const DEFAULT_GAIN: f64 = 0.5;
const MAX_GROUPS: usize = 32;
const MAX_NAME: usize = 64;
const QUEUE_REFILL: usize = 200;
const QUEUE_KEY: &str = "jukebox_queue";
const POLL: Duration = Duration::from_secs(1);
const SETTLE: Duration = Duration::from_secs(3);
const SEEK_SETTLE: Duration = Duration::from_secs(1);
const SEEK_TOLERANCE_SECS: u64 = 2;
const HEARTBEAT_TICKS: u32 = 10;
const IDLE_TICKS: u32 = 5;
const STREAM_LINK_SECS: i64 = 7 * 24 * 3600;
const RETRY_TICKS: u32 = 15;
const DISCOVER_EVERY: Duration = Duration::from_secs(45);
const FORGET_AFTER: Duration = Duration::from_secs(180);
const MAX_FAILURES: u32 = 3;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub current_index: usize,
    pub playing: bool,
    pub gain: f64,
    pub position: u64,
    pub position_ms: u64,
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
    pub locks: HashMap<String, airplay::Lock>,
    pub current: Option<(Device, bool)>,
    pub devices: Vec<Device>,
    pub groups: Vec<Group>,
    pub speakers: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    udn: String,
    name: String,
    location: String,
    #[serde(default = "dlna_protocol")]
    protocol: String,
}

fn dlna_protocol() -> String {
    "dlna".into()
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
    path: PathBuf,
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
    output: Option<SharedOutput>,
    known: Vec<Device>,
    groups: Vec<Group>,
    seen: HashMap<String, Instant>,
    failures: u32,
    resume: bool,
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
            position_ms: self.position_ms(),
            queue_length: self.queue.len(),
            revision: self.revision,
            output: self.saved.as_ref().map(|d| d.name.clone()),
            online: self.output.is_some(),
            timestamp: now_ms(),
        }
    }
}

struct Cache {
    snapshot: Snapshot,
    queue: Vec<String>,
    outputs: Outputs,
}

pub struct Jukebox {
    inner: Mutex<Inner>,
    cache: std::sync::RwLock<Option<Cache>>,
    events: broadcast::Sender<Snapshot>,
    plays: broadcast::Sender<PlayEvent>,
    saves: watch::Sender<Option<String>>,
    stop: watch::Sender<bool>,
    db: Db,
    crypto: Arc<Crypto>,
    port: u16,
    hub: Arc<snapcast::Hub>,
    pcm: Arc<browser::Pcm>,
    airplay: Arc<airplay::Scanner>,
    pairing: std::sync::Mutex<HashMap<String, (Instant, airplay::PendingPairing)>>,
    snapcast: bool,
    live: Live,
    seats: std::sync::Mutex<HashMap<String, Seat>>,
}

struct Seat {
    user: String,
    info: String,
    kind: &'static str,
    refs: usize,
}

fn merge_scan(g: &mut Inner, found: Vec<Device>) -> bool {
    let before: Vec<(String, String)> = g
        .known
        .iter()
        .map(|d| (d.udn.clone(), d.name.clone()))
        .collect();
    let now = Instant::now();
    for d in found {
        g.seen.insert(d.udn.clone(), now);
        match g.known.iter_mut().find(|k| k.udn == d.udn) {
            Some(k) => *k = d,
            None => g.known.push(d),
        }
    }
    let keep = g.saved.as_ref().map(|d| d.udn.clone());
    let seen = g.seen.clone();
    g.known.retain(|d| {
        d.protocol == snapcast::PROTOCOL
            || d.protocol == airplay::PROTOCOL
            || keep.as_deref() == Some(d.udn.as_str())
            || seen.get(&d.udn).is_some_and(|t| t.elapsed() < FORGET_AFTER)
    });
    let after: Vec<(String, String)> = g
        .known
        .iter()
        .map(|d| (d.udn.clone(), d.name.clone()))
        .collect();
    before != after
}

fn merge_snapcast(g: &mut Inner, found: Vec<Device>) -> bool {
    merge_protocol(g, snapcast::PROTOCOL, found)
}

fn merge_protocol(g: &mut Inner, protocol: &str, found: Vec<Device>) -> bool {
    let before: Vec<(String, String)> = g
        .known
        .iter()
        .filter(|d| d.protocol == protocol)
        .map(|d| (d.udn.clone(), d.name.clone()))
        .collect();
    let keep = g.saved.as_ref().map(|d| d.udn.clone());
    g.known.retain(|d| {
        d.protocol != protocol
            || keep.as_deref() == Some(d.udn.as_str())
            || found.iter().any(|f| f.udn == d.udn)
    });
    for d in found {
        match g.known.iter_mut().find(|k| k.udn == d.udn) {
            Some(k) => *k = d,
            None => g.known.push(d),
        }
    }
    let after: Vec<(String, String)> = g
        .known
        .iter()
        .filter(|d| d.protocol == protocol)
        .map(|d| (d.udn.clone(), d.name.clone()))
        .collect();
    before != after
}

fn no_output() -> ApiError {
    ApiError::generic("no jukebox output is configured")
}

fn offline() -> ApiError {
    ApiError::generic("the jukebox output is unreachable")
}

impl Jukebox {
    pub fn new(
        db: Db,
        crypto: Arc<Crypto>,
        port: u16,
        snapcast_port: Option<u16>,
        live: Live,
    ) -> Arc<Jukebox> {
        Arc::new(Jukebox {
            cache: std::sync::RwLock::new(None),
            inner: Mutex::new(Inner {
                queue: Vec::new(),
                index: 0,
                playing: false,
                base_ms: 0,
                anchor: None,
                gain: DEFAULT_GAIN,
                loaded: None,
                settled: Instant::now(),
                revision: 0,
                saved: None,
                output: None,
                known: Vec::new(),
                groups: Vec::new(),
                seen: HashMap::new(),
                failures: 0,
                resume: false,
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
            hub: snapcast::Hub::new(snapcast_port.unwrap_or(snapcast::DEFAULT_PORT)),
            pcm: Arc::new(browser::Pcm::default()),
            airplay: airplay::Scanner::new(),
            pairing: std::sync::Mutex::new(HashMap::new()),
            snapcast: snapcast_port.is_some(),
            live,
            seats: std::sync::Mutex::new(HashMap::new()),
        })
    }

    pub fn start(self: &Arc<Self>) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.snapcast {
            if let Err(e) = self.hub.start() {
                tracing::warn!("snapcast is unavailable: {e:#}");
            }
            let jb = self.clone();
            handle.spawn(async move { jb.watch_speakers().await });
        }
        if let Err(e) = self.airplay.start() {
            tracing::warn!("airplay discovery is unavailable: {e:#}");
        } else {
            let jb = self.clone();
            handle.spawn(async move { jb.watch_airplay().await });
        }
        let jb = self.clone();
        handle.spawn(async move {
            jb.restore().await;
            jb.poll().await;
        });
        let jb = self.clone();
        handle.spawn(async move { jb.save_loop().await });
        let jb = self.clone();
        handle.spawn(async move { jb.discover_loop().await });
    }

    pub fn shutdown(&self) {
        let _ = self.stop.send(true);
        if let Ok(g) = self.inner.try_lock()
            && let Some(o) = &g.output
        {
            o.close();
        }
        self.hub.shutdown();
        self.airplay.shutdown();
    }

    pub fn snapcast_port(&self) -> Option<u16> {
        self.hub.port()
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

    fn directory(&self, g: &Inner) -> Directory {
        Directory {
            hub: self.hub.clone(),
            pcm: self.pcm.clone(),
            airplay: self.airplay.clone(),
            known: g.known.clone(),
            groups: g.groups.clone(),
        }
    }

    async fn watch_speakers(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        let mut changes = self.hub.subscribe();
        loop {
            tokio::select! {
                _ = stop.changed() => return,
                changed = changes.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
            let found = self.hub.devices();
            let mut g = self.inner.lock().await;
            if merge_snapcast(&mut g, found) {
                self.rebuild(&g);
                self.live.notify();
            }
        }
    }

    async fn watch_airplay(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        let mut changes = self.airplay.subscribe();
        loop {
            tokio::select! {
                _ = stop.changed() => return,
                changed = changes.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
            let found = self.airplay.devices();
            let mut g = self.inner.lock().await;
            if merge_protocol(&mut g, airplay::PROTOCOL, found) {
                self.rebuild(&g);
                self.live.notify();
            }
        }
    }

    async fn restore_groups(&self) {
        let groups = self
            .db
            .run(|c| Ok(settings::get(c, GROUPS_KEY)?))
            .await
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<Vec<Group>>(&raw).ok())
            .unwrap_or_default();
        self.inner.lock().await.groups = groups;
    }

    async fn restore_gain(&self) {
        let gain = self
            .db
            .run(|c| Ok(settings::get(c, GAIN_KEY)?))
            .await
            .ok()
            .flatten()
            .and_then(|raw| raw.parse::<f64>().ok())
            .filter(|g| (0.0..=1.0).contains(g));
        if let Some(gain) = gain {
            self.inner.lock().await.gain = gain;
        }
    }

    async fn restore_airplay_auth(&self) {
        let all = self
            .db
            .run(|c| Ok(settings::get(c, AIRPLAY_AUTH_KEY)?))
            .await
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<HashMap<String, airplay::Auth>>(&raw).ok())
            .unwrap_or_default();
        self.airplay.load_auth(all);
    }

    async fn save_airplay_auth(&self) -> Result<(), ApiError> {
        let raw = serde_json::to_string(&self.airplay.export_auth())
            .map_err(|_| ApiError::generic("internal server error"))?;
        self.db
            .run(move |c| Ok(settings::set(c, AIRPLAY_AUTH_KEY, &raw)?))
            .await
    }

    async fn reopen_if_selected(&self, udn: &str) -> Result<(), ApiError> {
        let selected = self
            .inner
            .lock()
            .await
            .saved
            .as_ref()
            .is_some_and(|d| d.udn == udn);
        if selected {
            self.select(Some(udn), None).await?;
        }
        self.refresh().await;
        self.live.notify();
        Ok(())
    }

    fn airplay_address(&self, udn: &str) -> Result<std::net::SocketAddr, ApiError> {
        if airplay::id_of(udn).is_none() {
            return Err(ApiError::bad_param("id"));
        }
        self.airplay
            .address_of(udn)
            .ok_or_else(|| ApiError::not_found("renderer"))
    }

    pub async fn airplay_password(
        &self,
        udn: &str,
        password: Option<String>,
    ) -> Result<(), ApiError> {
        self.airplay_address(udn)?;
        let mut auth = self.airplay.auth(udn);
        auth.password = password.filter(|p| !p.is_empty());
        auth.pairing = None;
        self.airplay.set_auth(udn, auth);
        self.save_airplay_auth().await?;
        self.reopen_if_selected(udn).await
    }

    pub async fn airplay_forget(&self, udn: &str) -> Result<(), ApiError> {
        self.airplay_address(udn)?;
        self.airplay.set_auth(udn, airplay::Auth::default());
        self.save_airplay_auth().await?;
        self.reopen_if_selected(udn).await
    }

    pub async fn airplay_pair_start(&self, udn: &str) -> Result<(), ApiError> {
        let address = self.airplay_address(udn)?;
        let pending = tokio::task::spawn_blocking(move || airplay::session::begin_pairing(address))
            .await
            .map_err(|_| ApiError::generic("internal server error"))?
            .map_err(|e| {
                tracing::warn!("starting airplay pairing: {e:#}");
                ApiError::generic(format!("cannot start pairing: {e}"))
            })?;
        let mut map = self.pairing.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, (at, _)| at.elapsed() < PAIRING_WINDOW);
        map.insert(udn.to_string(), (Instant::now(), pending));
        Ok(())
    }

    pub async fn airplay_pair_finish(&self, udn: &str, pin: &str) -> Result<(), ApiError> {
        let pending = {
            let mut map = self.pairing.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(udn)
                .filter(|(at, _)| at.elapsed() < PAIRING_WINDOW)
                .map(|(_, pending)| pending)
        }
        .ok_or_else(|| ApiError::generic("start pairing first, the code is shown on the device"))?;
        let pin = pin.to_string();
        let pairing =
            tokio::task::spawn_blocking(move || airplay::session::finish_pairing(pending, &pin))
                .await
                .map_err(|_| ApiError::generic("internal server error"))?
                .map_err(|e| {
                    tracing::warn!("finishing airplay pairing: {e:#}");
                    ApiError::generic(format!("pairing failed: {e}"))
                })?;
        self.airplay.set_auth(
            udn,
            airplay::Auth {
                password: None,
                pairing: Some(pairing),
            },
        );
        self.save_airplay_auth().await?;
        self.reopen_if_selected(udn).await
    }

    async fn restore(&self) {
        self.restore_airplay_auth().await;
        self.restore_queue().await;
        self.restore_groups().await;
        self.restore_gain().await;
        let saved = self
            .db
            .run(|c| Ok(settings::get(c, OUTPUT_KEY)?))
            .await
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<Saved>(&raw).ok())
            .filter(|s| s.udn != snapcast::PROTOCOL);
        let Some(saved) = saved else {
            let mut g = self.inner.lock().await;
            self.publish(&mut g);
            return;
        };
        let device = Device {
            udn: saved.udn,
            name: saved.name,
            location: saved.location,
            protocol: saved.protocol,
        };
        let target = device.clone();
        let dir = self.directory(&*self.inner.lock().await);
        let renderer = tokio::task::spawn_blocking(move || output::locate(&target, &dir))
            .await
            .ok()
            .flatten();
        let mut g = self.inner.lock().await;
        if !matches!(device.protocol.as_str(), group::PROTOCOL | group::BROWSER)
            && !g.known.iter().any(|d| d.udn == device.udn)
        {
            g.known.push(device.clone());
        }
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
        let Some(output) = g.output.clone() else {
            return;
        };
        if g.queue.is_empty() {
            return;
        }
        let probe =
            tokio::task::spawn_blocking(move || anyhow::Ok((output.state()?, output.position()?)))
                .await;
        let Ok(Ok((state, position))) = probe else {
            return;
        };
        if state != State::Playing && state != State::Paused {
            return;
        }
        let Some(song_id) = position.song_id else {
            return;
        };
        let Some(index) = g.queue.iter().position(|id| *id == song_id) else {
            return;
        };
        tracing::info!("jukebox resumed track {index} of {}", g.queue.len());
        g.index = index;
        g.loaded = Some(g.queue[index].clone());
        g.base_ms = position.seconds.unwrap_or(0) * 1000;
        if state == State::Playing {
            g.playing = true;
            g.anchor = Some(Instant::now());
            g.settled = Instant::now();
        }
    }

    async fn attach(&self, g: &mut Inner, output: SharedOutput) {
        let probe = output.clone();
        let virtual_output = matches!(
            output.device().protocol.as_str(),
            snapcast::PROTOCOL | airplay::PROTOCOL | group::PROTOCOL | group::BROWSER
        );
        if virtual_output {
            let percent = (g.gain * 100.0).round() as u32;
            let _ = tokio::task::spawn_blocking(move || probe.set_volume(percent)).await;
        } else if let Ok(Ok(v)) = tokio::task::spawn_blocking(move || probe.volume()).await {
            g.gain = f64::from(v) / 100.0;
        }
        let device = output.device().clone();
        g.saved = Some(device.clone());
        if !matches!(device.protocol.as_str(), group::PROTOCOL | group::BROWSER)
            && !g.known.iter().any(|d| d.udn == device.udn)
        {
            g.known.push(device);
        }
        g.output = Some(output);
        g.failures = 0;
    }

    fn outputs_of(&self, g: &Inner) -> Outputs {
        let locks = g
            .known
            .iter()
            .filter(|d| d.protocol == airplay::PROTOCOL)
            .filter_map(|d| Some((d.udn.clone(), self.airplay.lock_of(&d.udn)?)))
            .collect();
        Outputs {
            locks,
            current: g.saved.clone().map(|d| (d, g.output.is_some())),
            devices: g.known.clone(),
            groups: g.groups.clone(),
            speakers: self.hub.speakers(),
        }
    }

    fn rebuild(&self, g: &Inner) {
        let cache = Cache {
            snapshot: g.snapshot(),
            queue: g.queue.clone(),
            outputs: self.outputs_of(g),
        };
        *self.cache.write().unwrap_or_else(|e| e.into_inner()) = Some(cache);
    }

    async fn refresh(&self) {
        let g = self.inner.lock().await;
        self.rebuild(&g);
    }

    fn live(mut snapshot: Snapshot) -> Snapshot {
        let now = now_ms();
        if snapshot.playing {
            snapshot.position_ms += (now - snapshot.timestamp).max(0) as u64;
            snapshot.position = snapshot.position_ms / 1000;
        }
        snapshot.timestamp = now;
        snapshot
    }

    fn publish(&self, g: &mut Inner) -> Snapshot {
        self.rebuild(g);
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
        if let Some(c) = self
            .cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return Self::live(c.snapshot.clone());
        }
        self.inner.lock().await.snapshot()
    }

    pub async fn clock(&self) -> Option<stream::Clock> {
        let g = self.inner.lock().await;
        g.output.as_ref().and_then(|o| o.clock())
    }

    pub async fn state(&self) -> (Snapshot, Vec<String>) {
        if let Some(c) = self
            .cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return (Self::live(c.snapshot.clone()), c.queue.clone());
        }
        let g = self.inner.lock().await;
        (g.snapshot(), g.queue.clone())
    }

    pub fn sign(&self, id: &str) -> String {
        let expires = crate::db::now() + STREAM_LINK_SECS;
        format!(
            "{expires}-{}",
            self.crypto.sign(&format!("jukebox:{id}:{expires}"))
        )
    }

    pub fn verify(&self, id: &str, token: &str) -> bool {
        let Some((expires, sig)) = token.split_once('-') else {
            return false;
        };
        let Ok(expires_at) = expires.parse::<i64>() else {
            return false;
        };
        let expected = self.crypto.sign(&format!("jukebox:{id}:{expires}"));
        crate::auth::constant_eq(&expected, sig) && expires_at >= crate::db::now()
    }

    async fn song(&self, id: &str) -> Result<Song, ApiError> {
        let id = id.to_string();
        self.db
            .run(move |c| {
                use rusqlite::OptionalExtension;
                c.query_row(
                    "SELECT s.title, s.display_artist, al.title, s.content_type, s.suffix, s.duration_ms, s.path
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
                            path: PathBuf::from(r.get::<_, String>(6)?),
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

    async fn with_output<T, F>(&self, g: &mut Inner, f: F) -> Result<T, ApiError>
    where
        T: Send + 'static,
        F: Fn(&dyn Output) -> anyhow::Result<T> + Clone + Send + 'static,
    {
        let Some(output) = g.output.clone() else {
            return Err(if g.saved.is_some() {
                offline()
            } else {
                no_output()
            });
        };
        let first = f.clone();
        match tokio::task::spawn_blocking(move || first(&*output)).await {
            Ok(Ok(v)) => return Ok(v),
            Ok(Err(e)) if output::is_unreachable(&e) => {
                tracing::warn!("jukebox output unreachable: {e:#}");
            }
            Ok(Err(e)) => {
                return Err(ApiError::generic(format!("jukebox output failed: {e:#}")));
            }
            Err(_) => return Err(ApiError::generic("internal server error")),
        }
        let saved = g.saved.clone().ok_or_else(no_output)?;
        let dir = self.directory(g);
        let fresh = tokio::task::spawn_blocking(move || output::locate(&saved, &dir))
            .await
            .ok()
            .flatten();
        let Some(fresh) = fresh else {
            g.output = None;
            return Err(offline());
        };
        g.output = Some(fresh.clone());
        match tokio::task::spawn_blocking(move || f(&*fresh)).await {
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
        let stream_path = format!("/jukebox/stream/{id}/{}.{}", self.sign(id), song.suffix);
        let (id, port) = (id.to_string(), self.port);
        self.with_output(g, move |o| {
            o.load(
                &Track {
                    id: &id,
                    path: &song.path,
                    title: &song.title,
                    artist: &song.artist,
                    album: &song.album,
                    content_type: &song.content_type,
                    duration_secs: song.duration_secs,
                    stream_path: &stream_path,
                    port,
                },
                offset,
            )
        })
        .await?;
        Ok(duration_ms)
    }

    pub async fn start_playback(&self) -> Result<Snapshot, ApiError> {
        let mut g = self.inner.lock().await;
        if g.queue.is_empty() {
            return Ok(self.publish(&mut g));
        }
        if g.output.is_none() {
            return Err(if g.saved.is_some() {
                offline()
            } else {
                no_output()
            });
        }
        let index = g.index.min(g.queue.len() - 1);
        g.index = index;
        let same = g.loaded.as_deref() == Some(g.queue[index].as_str());
        let resumed = same && self.with_output(&mut g, |o| o.resume()).await.is_ok();
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
            if let Err(e) = self.with_output(&mut g, |o| o.pause()).await {
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
                    .with_output(&mut g, move |o| o.seek(offset))
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
        if was_loaded && let Err(e) = self.with_output(g, |o| o.stop()).await {
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

    pub async fn move_item(&self, from: usize, to: usize) -> Result<Snapshot, ApiError> {
        let mut g = self.inner.lock().await;
        if from >= g.queue.len() {
            return Err(ApiError::bad_param("from"));
        }
        if to >= g.queue.len() {
            return Err(ApiError::bad_param("to"));
        }
        if from != to {
            let id = g.queue.remove(from);
            g.queue.insert(to, id);
            if g.index == from {
                g.index = to;
            } else if from < g.index && to >= g.index {
                g.index -= 1;
            } else if from > g.index && to <= g.index {
                g.index += 1;
            }
            g.revision += 1;
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
        let raw = gain.to_string();
        let _ = self
            .db
            .run(move |c| Ok(settings::set(c, GAIN_KEY, &raw)?))
            .await;
        if g.output.is_some() {
            let percent = (gain * 100.0).round() as u32;
            if let Err(e) = self
                .with_output(&mut g, move |o| o.set_volume(percent))
                .await
            {
                tracing::warn!("setting jukebox volume: {e}");
            }
        }
        Ok(self.publish(&mut g))
    }

    pub async fn outputs(&self) -> Outputs {
        if let Some(c) = self
            .cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return c.outputs.clone();
        }
        let g = self.inner.lock().await;
        self.outputs_of(&g)
    }

    pub async fn scan(&self) -> Result<Vec<Device>, ApiError> {
        let found = tokio::task::spawn_blocking(output::discover)
            .await
            .map_err(|_| ApiError::generic("internal server error"))?;
        let mut g = self.inner.lock().await;
        merge_scan(&mut g, found);
        merge_snapcast(&mut g, self.hub.devices());
        merge_protocol(&mut g, airplay::PROTOCOL, self.airplay.devices());
        self.rebuild(&g);
        self.live.notify();
        Ok(g.known.clone())
    }

    async fn discover_loop(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        let mut first = true;
        loop {
            let wait = if first {
                Duration::from_secs(1)
            } else {
                DISCOVER_EVERY
            };
            first = false;
            tokio::select! {
                _ = stop.changed() => return,
                _ = tokio::time::sleep(wait) => {}
            }
            let found = tokio::task::spawn_blocking(output::discover)
                .await
                .unwrap_or_default();
            let mut g = self.inner.lock().await;
            if merge_scan(&mut g, found) {
                self.rebuild(&g);
                self.live.notify();
            }
        }
    }

    pub fn pcm(&self, id: &str) -> browser::Frames {
        self.pcm.sender(id)
    }

    pub fn seat_join(&self, user: &str, id: &str, info: &str, kind: &str) -> bool {
        if !browser::valid_id(id) {
            return false;
        }
        {
            let mut seats = self.seats.lock().unwrap_or_else(|e| e.into_inner());
            let seat = seats.entry(id.to_string()).or_insert_with(|| Seat {
                user: user.to_string(),
                info: String::new(),
                kind: "browser",
                refs: 0,
            });
            if seat.user != user {
                return false;
            }
            seat.refs += 1;
            seat.info = browser::clean_info(info);
            seat.kind = browser::clean_kind(kind);
        }
        self.refresh_seats();
        true
    }

    pub fn seat_leave(&self, id: &str) {
        {
            let mut seats = self.seats.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(seat) = seats.get_mut(id) {
                seat.refs = seat.refs.saturating_sub(1);
                if seat.refs == 0 {
                    seats.remove(id);
                }
            }
        }
        self.refresh_seats();
    }

    pub fn seats_of(&self, user: &str) -> Vec<(String, String, &'static str)> {
        let seats = self.seats.lock().unwrap_or_else(|e| e.into_inner());
        let mut mine: Vec<(String, String, &'static str)> = seats
            .iter()
            .filter(|(_, s)| s.user == user)
            .map(|(id, s)| (id.clone(), s.info.clone(), s.kind))
            .collect();
        mine.sort();
        mine
    }

    fn refresh_seats(&self) {
        let snapshot = self
            .cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|c| c.snapshot.clone());
        match snapshot {
            Some(snapshot) => {
                let _ = self.events.send(snapshot);
            }
            None => self.live.notify(),
        }
    }

    pub async fn select(&self, udn: Option<&str>, url: Option<&str>) -> Result<(), ApiError> {
        let renderer = match (url, udn) {
            (Some(url), _) => {
                let url = url.to_string();
                Some(
                    tokio::task::spawn_blocking(move || output::connect(&url))
                        .await
                        .map_err(|_| ApiError::generic("internal server error"))?
                        .map_err(|e| {
                            tracing::warn!("cannot use renderer: {e:#}");
                            ApiError::generic("cannot use renderer")
                        })?,
                )
            }
            (None, Some(udn)) => {
                let (device, dir) = {
                    let g = self.inner.lock().await;
                    let device = match udn.strip_prefix(group::BROWSER_PREFIX) {
                        Some(id) if browser::valid_id(id) => browser::device(id),
                        Some(_) => return Err(ApiError::not_found("renderer")),
                        None => g
                            .known
                            .iter()
                            .cloned()
                            .chain(g.groups.iter().map(Group::device))
                            .find(|d| d.udn == udn)
                            .ok_or_else(|| ApiError::not_found("renderer"))?,
                    };
                    (device, self.directory(&g))
                };
                Some(
                    tokio::task::spawn_blocking(move || output::open(&device, &dir))
                        .await
                        .map_err(|_| ApiError::generic("internal server error"))?
                        .map_err(|e| {
                            tracing::warn!("cannot use renderer: {e:#}");
                            match e.downcast_ref::<airplay::NeedsAuth>() {
                                Some(needs) => {
                                    ApiError::generic(format!("cannot use renderer: {needs}"))
                                }
                                None => ApiError::generic("cannot use renderer"),
                            }
                        })?,
                )
            }
            (None, None) => None,
        };

        let mut g = self.inner.lock().await;
        let resume = (g.playing && g.loaded.is_some() && renderer.is_some())
            .then(|| (g.index, g.position_ms() / 1000));
        self.finish(&mut g, false);
        if g.output.is_some() && g.loaded.is_some() {
            let _ = self.with_output(&mut g, |o| o.stop()).await;
        }
        g.halt();
        g.loaded = None;
        if let Some(old) = g.output.take() {
            tokio::task::spawn_blocking(move || old.close());
        }
        g.saved = None;

        let db = self.db.clone();
        match renderer {
            Some(r) if r.device().protocol == group::BROWSER => {
                db.run(|c| Ok(settings::delete(c, OUTPUT_KEY)?)).await?;
                self.attach(&mut g, r).await;
            }
            Some(r) => {
                let saved = serde_json::to_string(&Saved {
                    udn: r.device().udn.clone(),
                    name: r.device().name.clone(),
                    location: r.device().location.clone(),
                    protocol: r.device().protocol.clone(),
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
        if let Some((index, secs)) = resume
            && g.output.is_some()
            && index < g.queue.len()
            && let Err(e) = self.load(&mut g, index, secs).await
        {
            tracing::warn!("resuming on the new output: {e}");
        }
        self.publish(&mut g);
        self.live.notify();
        Ok(())
    }

    pub async fn save_group(
        &self,
        id: Option<&str>,
        name: &str,
        members: Vec<(String, i32)>,
    ) -> Result<Group, ApiError> {
        let name: String = name.trim().chars().take(MAX_NAME).collect();
        if name.is_empty() {
            return Err(ApiError::bad_param("name"));
        }
        if members.is_empty() || members.len() > group::MAX_MEMBERS {
            return Err(ApiError::bad_param("member"));
        }
        let mut g = self.inner.lock().await;
        let mut built: Vec<group::Member> = Vec::new();
        for (udn, delay) in members {
            if built.iter().any(|m| m.udn == udn) {
                return Err(ApiError::bad_param("member"));
            }
            if udn.starts_with(group::BROWSER_PREFIX) {
                built.push(
                    group::Member::browser(&udn, delay)
                        .ok_or_else(|| ApiError::bad_param("member"))?,
                );
                continue;
            }
            let device = g
                .known
                .iter()
                .find(|d| d.udn == udn)
                .cloned()
                .or_else(|| {
                    g.groups
                        .iter()
                        .flat_map(|x| x.members.iter())
                        .find(|m| m.udn == udn)
                        .map(group::Member::device)
                })
                .ok_or_else(|| ApiError::not_found("renderer"))?;
            if device.protocol == group::PROTOCOL {
                return Err(ApiError::bad_param("member"));
            }
            built.push(group::Member {
                udn,
                name: device.name,
                location: device.location,
                protocol: device.protocol,
                delay_ms: delay.clamp(group::MIN_DELAY_MS, group::MAX_DELAY_MS),
            });
        }
        let saved = match id {
            Some(id) => {
                let existing = g
                    .groups
                    .iter_mut()
                    .find(|x| x.id == id)
                    .ok_or_else(|| ApiError::not_found("group"))?;
                existing.name = name;
                existing.members = built;
                existing.clone()
            }
            None => {
                if g.groups.len() >= MAX_GROUPS {
                    return Err(ApiError::bad_param("name"));
                }
                let created = Group {
                    id: crate::ids::random(),
                    name,
                    members: built,
                };
                g.groups.push(created.clone());
                created
            }
        };
        let raw = serde_json::to_string(&g.groups)
            .map_err(|_| ApiError::generic("internal server error"))?;
        let udn = saved.device().udn;
        let selected = g.saved.as_ref().is_some_and(|s| s.udn == udn);
        let mut live = false;
        if selected && g.output.is_some() {
            let dir = self.directory(&g);
            let current = saved.clone();
            match self
                .with_output(&mut g, move |o| o.set_members(&current, &dir))
                .await
            {
                Ok(done) => live = done,
                Err(e) => tracing::warn!("applying group members: {e}"),
            }
        }
        drop(g);
        self.db
            .run(move |c| Ok(settings::set(c, GROUPS_KEY, &raw)?))
            .await?;
        if selected && !live {
            self.select(Some(&udn), None).await?;
        }
        self.refresh().await;
        self.live.notify();
        Ok(saved)
    }

    pub async fn delete_group(&self, id: &str) -> Result<(), ApiError> {
        let mut g = self.inner.lock().await;
        let before = g.groups.len();
        g.groups.retain(|x| x.id != id);
        if g.groups.len() == before {
            return Err(ApiError::not_found("group"));
        }
        let raw = serde_json::to_string(&g.groups)
            .map_err(|_| ApiError::generic("internal server error"))?;
        let udn = format!("{}{id}", group::PREFIX);
        let selected = g.saved.as_ref().is_some_and(|s| s.udn == udn);
        drop(g);
        self.db
            .run(move |c| Ok(settings::set(c, GROUPS_KEY, &raw)?))
            .await?;
        if selected {
            self.select(None, None).await?;
        }
        self.refresh().await;
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

        let Some(output) = g.output.clone() else {
            if g.saved.is_some() && g.ticks % RETRY_TICKS == 0 {
                let saved = g.saved.clone();
                let dir = self.directory(&g);
                drop(g);
                self.retry(saved, dir).await;
            }
            return;
        };
        if !g.playing && g.ticks % IDLE_TICKS != 0 {
            return;
        }

        let probe = tokio::task::spawn_blocking(move || {
            let state = output.state()?;
            let position = output.position().ok();
            let volume = output.volume().ok();
            anyhow::Ok((state, position, volume))
        })
        .await;

        let (state, position, volume) = match probe {
            Ok(Ok(v)) => v,
            _ => {
                g.failures += 1;
                if g.failures >= MAX_FAILURES {
                    tracing::warn!("jukebox output stopped responding");
                    g.resume = g.playing;
                    g.output = None;
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
            match state {
                State::Playing => {
                    if let Some(secs) = position.and_then(|p| p.seconds) {
                        g.base_ms = secs * 1000;
                        g.anchor = Some(Instant::now());
                    }
                }
                State::Paused if g.settled.elapsed() > SETTLE => {
                    if let Some(secs) = position.and_then(|p| p.seconds) {
                        g.base_ms = secs * 1000;
                    }
                    g.anchor = None;
                    g.playing = false;
                }
                State::Stopped if g.settled.elapsed() > SETTLE => {
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

    async fn retry(&self, saved: Option<Device>, dir: Directory) {
        let Some(saved) = saved else {
            return;
        };
        let target = saved.clone();
        let found = tokio::task::spawn_blocking(move || output::locate(&target, &dir))
            .await
            .ok()
            .flatten();
        let Some(renderer) = found else {
            return;
        };
        let mut g = self.inner.lock().await;
        if g.output.is_none() && g.saved.as_ref().is_some_and(|s| s.udn == saved.udn) {
            tracing::info!("jukebox output is back");
            self.attach(&mut g, renderer).await;
            if g.resume && g.index < g.queue.len() {
                g.resume = false;
                let (index, secs) = (g.index, g.base_ms / 1000);
                if let Err(e) = self.load(&mut g, index, secs).await {
                    tracing::warn!("resuming after the output came back: {e}");
                }
            }
            self.publish(&mut g);
            self.live.notify();
        }
    }
}
