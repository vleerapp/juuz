use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use tokio::sync::watch;

use super::dlna::Device;
use super::output::{Output, Position, State, Track, Unreachable};
use super::stream::{Sink, Stream, now_us};
use crate::pcm::{CHANNELS, RATE};

pub const DEFAULT_PORT: u16 = 1704;
pub const PROTOCOL: &str = "snapcast";
pub const PREFIX: &str = "snapcast:";
pub const BUFFER_MS: u32 = 1000;
pub const DEFAULT_VOLUME: u8 = 50;
const MAX_CLIENTS: usize = 64;
const MAX_BODY: usize = 64 * 1024;
const QUEUE_DEPTH: usize = 512;
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

const HELLO: u16 = 5;
const TIME: u16 = 4;
const SETTINGS: u16 = 3;
const CHUNK: u16 = 2;
const CODEC: u16 = 1;
const HEADER_LEN: usize = 26;

pub fn key_of(udn: &str) -> Option<&str> {
    udn.strip_prefix(PREFIX).filter(|k| !k.is_empty())
}

fn device(key: &str, name: &str, address: &str) -> Device {
    Device {
        udn: format!("{PREFIX}{key}"),
        name: name.into(),
        location: address.into(),
        protocol: PROTOCOL.into(),
    }
}

fn frame(kind: u16, refers_to: u16, body: &[u8]) -> Vec<u8> {
    static IDS: AtomicU64 = AtomicU64::new(1);
    let id = IDS.fetch_add(1, Ordering::Relaxed) as u16;
    let sent = now_us();
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&refers_to.to_le_bytes());
    out.extend_from_slice(&((sent / 1_000_000) as i32).to_le_bytes());
    out.extend_from_slice(&((sent % 1_000_000) as i32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    out
}

fn wav_header() -> Vec<u8> {
    let channels = CHANNELS as u16;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&36u32.to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&RATE.to_le_bytes());
    h.extend_from_slice(&(RATE * u32::from(channels) * 2).to_le_bytes());
    h.extend_from_slice(&(channels * 2).to_le_bytes());
    h.extend_from_slice(&16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&0u32.to_le_bytes());
    h
}

fn codec_frame() -> Vec<u8> {
    let wav = wav_header();
    let mut body = Vec::new();
    body.extend_from_slice(&3u32.to_le_bytes());
    body.extend_from_slice(b"pcm");
    body.extend_from_slice(&(wav.len() as u32).to_le_bytes());
    body.extend_from_slice(&wav);
    frame(CODEC, 0, &body)
}

#[derive(Clone, Copy)]
struct ClientState {
    volume: u8,
    muted: bool,
    latency: i32,
}

impl Default for ClientState {
    fn default() -> Self {
        Self {
            volume: DEFAULT_VOLUME,
            muted: false,
            latency: 0,
        }
    }
}

fn settings_frame(state: ClientState, refers_to: u16) -> Vec<u8> {
    let ClientState {
        volume,
        muted,
        latency,
    } = state;
    let json = format!(
        "{{\"bufferMs\":{BUFFER_MS},\"latency\":{latency},\"muted\":{muted},\"volume\":{volume}}}"
    );
    let mut body = Vec::new();
    body.extend_from_slice(&(json.len() as u32).to_le_bytes());
    body.extend_from_slice(json.as_bytes());
    frame(SETTINGS, refers_to, &body)
}

pub fn chunk_frame(timestamp_us: i64, pcm: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(12 + pcm.len());
    body.extend_from_slice(&((timestamp_us / 1_000_000) as i32).to_le_bytes());
    body.extend_from_slice(&((timestamp_us % 1_000_000) as i32).to_le_bytes());
    body.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    body.extend_from_slice(pcm);
    frame(CHUNK, 0, &body)
}

struct ClientHandle {
    id: u64,
    key: String,
    name: String,
    address: String,
    tx: SyncSender<Arc<Vec<u8>>>,
    ready: Arc<AtomicBool>,
    socket: TcpStream,
}

struct Listening {
    stop: Arc<AtomicBool>,
    port: u16,
}

struct HubInner {
    listening: Option<Listening>,
    advert: Option<mdns_sd::ServiceDaemon>,
    clients: Vec<ClientHandle>,
    volumes: HashMap<String, ClientState>,
    next_id: u64,
}

pub struct Hub {
    port: u16,
    inner: Mutex<HubInner>,
    changes: watch::Sender<u64>,
}

impl Hub {
    pub fn new(port: u16) -> Arc<Hub> {
        Arc::new(Hub {
            port,
            inner: Mutex::new(HubInner {
                listening: None,
                advert: None,
                clients: Vec::new(),
                volumes: HashMap::new(),
                next_id: 1,
            }),
            changes: watch::channel(0).0,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HubInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn changed(&self) {
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    pub fn port(&self) -> Option<u16> {
        self.lock().listening.as_ref().map(|l| l.port)
    }

    pub fn devices(&self) -> Vec<Device> {
        let g = self.lock();
        let mut out: Vec<Device> = Vec::new();
        for c in g.clients.iter().filter(|c| c.ready.load(Ordering::Acquire)) {
            if !out.iter().any(|d| key_of(&d.udn) == Some(c.key.as_str())) {
                out.push(device(&c.key, &c.name, &c.address));
            }
        }
        out
    }

    pub fn speakers(&self) -> Vec<String> {
        self.devices().into_iter().map(|d| d.name).collect()
    }

    pub fn online(&self, key: &str) -> bool {
        self.lock()
            .clients
            .iter()
            .any(|c| c.key == key && c.ready.load(Ordering::Acquire))
    }

    pub fn start(self: &Arc<Self>) -> Result<()> {
        let mut g = self.lock();
        if g.listening.is_some() {
            return Ok(());
        }
        let listener = TcpListener::bind(("0.0.0.0", self.port))
            .map_err(|e| anyhow!("cannot listen on port {}: {e}", self.port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        g.listening = Some(Listening {
            stop: stop.clone(),
            port,
        });
        g.advert = if self.port == 0 {
            None
        } else {
            advertise(port)
        };
        drop(g);
        let hub = self.clone();
        std::thread::Builder::new()
            .name("snapcast-accept".into())
            .spawn(move || hub.accept(listener, stop))?;
        tracing::info!("snapcast server listening on port {port}");
        Ok(())
    }

    pub fn shutdown(&self) {
        let mut g = self.lock();
        if let Some(l) = g.listening.take() {
            l.stop.store(true, Ordering::Release);
        }
        if let Some(advert) = g.advert.take() {
            withdraw(advert);
        }
        for c in g.clients.drain(..) {
            let _ = c.socket.shutdown(Shutdown::Both);
        }
    }

    fn accept(self: Arc<Self>, listener: TcpListener, stop: Arc<AtomicBool>) {
        while !stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((socket, peer)) => {
                    if self.lock().clients.len() >= MAX_CLIENTS {
                        continue;
                    }
                    tracing::debug!("snapcast connection from {peer}");
                    let hub = self.clone();
                    let _ = std::thread::Builder::new()
                        .name("snapcast-client".into())
                        .spawn(move || hub.serve(socket));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(250)),
            }
        }
    }

    fn serve(self: Arc<Self>, socket: TcpStream) {
        let _ = socket.set_nodelay(true);
        let _ = socket.set_nonblocking(false);
        let _ = socket.set_read_timeout(Some(IDLE_TIMEOUT));
        let (tx, rx) = sync_channel::<Arc<Vec<u8>>>(QUEUE_DEPTH);
        let ready = Arc::new(AtomicBool::new(false));
        let Ok(writer_socket) = socket.try_clone() else {
            return;
        };
        let Ok(handle_socket) = socket.try_clone() else {
            return;
        };
        let address = socket
            .peer_addr()
            .map(|a| a.ip().to_string())
            .unwrap_or_default();
        let id = {
            let mut g = self.lock();
            let id = g.next_id;
            g.next_id += 1;
            g.clients.push(ClientHandle {
                id,
                key: format!("pending-{id}"),
                name: String::new(),
                address,
                tx: tx.clone(),
                ready: ready.clone(),
                socket: handle_socket,
            });
            id
        };
        let writer = std::thread::Builder::new()
            .name("snapcast-writer".into())
            .spawn(move || {
                let mut socket = writer_socket;
                while let Ok(buf) = rx.recv() {
                    if socket.write_all(&buf).is_err() {
                        break;
                    }
                }
                let _ = socket.shutdown(Shutdown::Both);
            });
        let result = self.read_loop(socket, id, &tx, &ready);
        if let Err(e) = result {
            tracing::debug!("snapcast client {id} left: {e}");
        }
        self.lock().clients.retain(|c| c.id != id);
        if ready.load(Ordering::Acquire) {
            self.changed();
        }
        drop(tx);
        if let Ok(w) = writer {
            let _ = w.join();
        }
    }

    fn read_loop(
        &self,
        mut socket: TcpStream,
        id: u64,
        tx: &SyncSender<Arc<Vec<u8>>>,
        ready: &AtomicBool,
    ) -> Result<()> {
        loop {
            let mut head = [0u8; HEADER_LEN];
            socket.read_exact(&mut head)?;
            let received = now_us();
            let kind = u16::from_le_bytes([head[0], head[1]]);
            let msg_id = u16::from_le_bytes([head[2], head[3]]);
            let sent_sec = i32::from_le_bytes([head[6], head[7], head[8], head[9]]);
            let sent_usec = i32::from_le_bytes([head[10], head[11], head[12], head[13]]);
            let size = u32::from_le_bytes([head[22], head[23], head[24], head[25]]) as usize;
            if size > MAX_BODY {
                bail!("oversized message");
            }
            let mut body = vec![0u8; size];
            socket.read_exact(&mut body)?;
            match kind {
                TIME => {
                    let sent = i64::from(sent_sec) * 1_000_000 + i64::from(sent_usec);
                    let latency = received - sent;
                    let mut reply = Vec::with_capacity(8);
                    reply.extend_from_slice(&((latency / 1_000_000) as i32).to_le_bytes());
                    reply.extend_from_slice(&((latency % 1_000_000) as i32).to_le_bytes());
                    queue(tx, frame(TIME, msg_id, &reply))?;
                }
                HELLO => {
                    let (key, name) = hello_identity(&body, id);
                    let state = {
                        let mut g = self.lock();
                        let stale: Vec<ClientHandle> = {
                            let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut g.clients)
                                .into_iter()
                                .partition(|c| c.id != id && c.key == key);
                            g.clients = kept;
                            gone
                        };
                        for c in stale {
                            let _ = c.socket.shutdown(Shutdown::Both);
                        }
                        if let Some(c) = g.clients.iter_mut().find(|c| c.id == id) {
                            c.key = key.clone();
                            c.name = name.clone();
                        }
                        *g.volumes.entry(key.clone()).or_default()
                    };
                    queue(tx, settings_frame(state, msg_id))?;
                    queue(tx, codec_frame())?;
                    ready.store(true, Ordering::Release);
                    self.changed();
                    tracing::info!("snapcast speaker connected: {name}");
                }
                _ => {}
            }
        }
    }

    pub fn send(&self, key: &str, buf: Vec<u8>) {
        let buf = Arc::new(buf);
        let mut g = self.lock();
        g.clients.retain(|c| {
            if c.key != key || !c.ready.load(Ordering::Acquire) {
                return true;
            }
            match c.tx.try_send(buf.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    let _ = c.socket.shutdown(Shutdown::Both);
                    false
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
    }

    fn update(&self, key: &str, change: impl FnOnce(&mut ClientState)) {
        let state = {
            let mut g = self.lock();
            let entry = g.volumes.entry(key.to_string()).or_default();
            change(entry);
            *entry
        };
        self.send(key, settings_frame(state, 0));
    }

    pub fn set_volume(&self, key: &str, percent: u32) {
        self.update(key, |s| s.volume = percent.min(100) as u8);
    }

    pub fn set_latency(&self, key: &str, latency_ms: i32) {
        self.update(key, |s| s.latency = latency_ms);
    }

    pub fn volume(&self, key: &str) -> u32 {
        self.lock()
            .volumes
            .get(key)
            .map_or(u32::from(DEFAULT_VOLUME), |s| u32::from(s.volume))
    }
}

const ADVERT_NAME: &str = "juuz._snapcast._tcp.local.";

fn lan_addresses() -> Vec<std::net::IpAddr> {
    const VIRTUAL: [&str; 11] = [
        "docker", "br-", "veth", "virbr", "lxc", "cni", "flannel", "tun", "tap", "wg", "utun",
    ];
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    ifaces
        .into_iter()
        .filter(|i| {
            !VIRTUAL.iter().any(|v| i.name.starts_with(v)) && !i.name.starts_with("tailscale")
        })
        .filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_link_local() => {
                Some(std::net::IpAddr::V4(ip))
            }
            _ => None,
        })
        .collect()
}

fn advertise(port: u16) -> Option<mdns_sd::ServiceDaemon> {
    let daemon = mdns_sd::ServiceDaemon::new().ok()?;
    let addrs = lan_addresses();
    let info = mdns_sd::ServiceInfo::new(
        "_snapcast._tcp.local.",
        "juuz",
        "juuz.local.",
        addrs.as_slice(),
        port,
        None::<std::collections::HashMap<String, String>>,
    )
    .ok()?;
    let info = if addrs.is_empty() {
        info.enable_addr_auto()
    } else {
        info
    };
    tracing::info!("announcing snapcast on {addrs:?}");
    match daemon.register(info) {
        Ok(()) => Some(daemon),
        Err(e) => {
            tracing::debug!("snapcast advertisement failed: {e}");
            None
        }
    }
}

fn withdraw(daemon: mdns_sd::ServiceDaemon) {
    if let Ok(done) = daemon.unregister(ADVERT_NAME) {
        let _ = done.recv_timeout(Duration::from_millis(500));
    }
    let _ = daemon.shutdown();
}

fn queue(tx: &SyncSender<Arc<Vec<u8>>>, buf: Vec<u8>) -> Result<()> {
    tx.try_send(Arc::new(buf))
        .map_err(|_| anyhow!("client too slow"))
}

fn hello_identity(body: &[u8], fallback: u64) -> (String, String) {
    let json = body.get(4..).unwrap_or_default();
    let value: serde_json::Value = serde_json::from_slice(json).unwrap_or_default();
    let text = |k: &str| value[k].as_str().filter(|s| !s.is_empty());
    let key: String = ["ID", "MAC", "HostName"]
        .iter()
        .find_map(|k| text(k))
        .map_or_else(|| format!("client-{fallback}"), str::to_lowercase)
        .chars()
        .take(64)
        .collect();
    let mut name: String = ["HostName", "ClientName", "ID"]
        .iter()
        .find_map(|k| text(k))
        .unwrap_or("speaker")
        .chars()
        .take(64)
        .collect();
    if let Some(n) = value["Instance"].as_u64().filter(|n| *n > 1) {
        name = format!("{name} #{n}");
    }
    (key, name)
}

pub struct Emitter {
    hub: Arc<Hub>,
    keys: Vec<String>,
}

impl Sink for Emitter {
    fn push(&self, due_us: i64, pcm: &[u8]) {
        for key in &self.keys {
            self.hub.send(key, chunk_frame(due_us, pcm));
        }
    }
}

pub fn emitter(hub: Arc<Hub>, keys: Vec<String>) -> Arc<Emitter> {
    Arc::new(Emitter { hub, keys })
}

pub struct SnapcastOutput {
    device: Device,
    key: String,
    hub: Arc<Hub>,
    stream: Stream,
}

impl SnapcastOutput {
    pub fn connect(hub: &Arc<Hub>, device: &Device) -> Result<SnapcastOutput> {
        let key = key_of(&device.udn)
            .ok_or_else(|| anyhow!("not a snapcast speaker"))?
            .to_string();
        if !hub.online(&key) {
            return Err(Unreachable(format!("{} is not connected", device.name)).into());
        }
        let emit = emitter(hub.clone(), vec![key.clone()]);
        Ok(SnapcastOutput {
            device: device.clone(),
            key,
            hub: hub.clone(),
            stream: Stream::spawn("snapcast-engine", u64::from(BUFFER_MS), emit)?,
        })
    }
}

impl Output for SnapcastOutput {
    fn device(&self) -> &Device {
        &self.device
    }

    fn load(&self, track: &Track, offset: u64) -> Result<()> {
        self.stream
            .play(Some(track.id.to_string()), track.path.to_path_buf(), offset)
    }

    fn resume(&self) -> Result<()> {
        self.stream.resume();
        Ok(())
    }

    fn pause(&self) -> Result<()> {
        self.stream.pause();
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        self.stream.stop();
        Ok(())
    }

    fn seek(&self, secs: u64) -> Result<()> {
        self.stream.seek(secs)
    }

    fn state(&self) -> Result<State> {
        Ok(self.stream.state())
    }

    fn position(&self) -> Result<Position> {
        Ok(self.stream.position())
    }

    fn clock(&self) -> Option<super::stream::Clock> {
        self.stream.clock()
    }

    fn volume(&self) -> Result<u32> {
        Ok(self.hub.volume(&self.key))
    }

    fn set_volume(&self, percent: u32) -> Result<()> {
        self.hub.set_volume(&self.key, percent);
        Ok(())
    }

    fn close(&self) {
        self.stream.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_26_bytes_and_sized() {
        let f = frame(CHUNK, 7, &[1, 2, 3]);
        assert_eq!(f.len(), HEADER_LEN + 3);
        assert_eq!(u16::from_le_bytes([f[0], f[1]]), CHUNK);
        assert_eq!(u16::from_le_bytes([f[4], f[5]]), 7);
        assert_eq!(u32::from_le_bytes([f[22], f[23], f[24], f[25]]), 3);
    }

    #[test]
    fn wav_header_describes_the_stream() {
        let h = wav_header();
        assert_eq!(h.len(), 44);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(&h[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes([h[24], h[25], h[26], h[27]]), RATE);
        assert_eq!(&h[36..40], b"data");
    }

    #[test]
    fn speakers_are_keyed_by_id_and_named_by_host() {
        let json = br#"{"ClientName":"Snapclient","HostName":"Kitchen","ID":"AA:BB","Instance":2}"#;
        let mut body = (json.len() as u32).to_le_bytes().to_vec();
        body.extend_from_slice(json);
        assert_eq!(
            hello_identity(&body, 9),
            ("aa:bb".to_string(), "Kitchen #2".to_string())
        );
        assert_eq!(
            hello_identity(&[], 9),
            ("client-9".to_string(), "speaker".to_string())
        );
    }
}
