use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use tokio::sync::broadcast;

use super::dlna::Device;
use super::group::{BROWSER, BROWSER_PREFIX};
use super::output::{Directory, Output, Position, State, Track};
use super::snapcast::DEFAULT_VOLUME;
use super::stream::{Sink, Stream, now_us, unix_us};
use crate::pcm::RATE;

pub const MAX_INFO: usize = 48;
pub const LEAD_MS: u64 = 500;
const AUDIO: u8 = 1;
const FLUSH: u8 = 2;
const HEADER: usize = 16;

pub type Frames = broadcast::Sender<Arc<Vec<u8>>>;

#[derive(Default)]
pub struct Pcm(Mutex<HashMap<String, Frames>>);

impl Pcm {
    pub fn sender(&self, id: &str) -> Frames {
        let mut all = self.0.lock().unwrap_or_else(|e| e.into_inner());
        all.entry(id.to_string())
            .or_insert_with(|| broadcast::channel(64).0)
            .clone()
    }
}

pub struct BrowserSink {
    tx: Frames,
    latency_ms: Arc<AtomicI32>,
}

impl BrowserSink {
    pub fn new(tx: Frames, latency_ms: Arc<AtomicI32>) -> BrowserSink {
        BrowserSink { tx, latency_ms }
    }

    fn frame(kind: u8, latency_ms: i32, unix_due: i64, pcm: &[u8]) -> Arc<Vec<u8>> {
        let mut out = Vec::with_capacity(HEADER + pcm.len());
        out.push(kind);
        out.extend_from_slice(&((RATE / 100) as u16).to_le_bytes());
        out.push(0);
        out.extend_from_slice(&latency_ms.to_le_bytes());
        out.extend_from_slice(&unix_due.to_le_bytes());
        out.extend_from_slice(pcm);
        Arc::new(out)
    }
}

impl Sink for BrowserSink {
    fn push(&self, due_us: i64, pcm: &[u8]) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        let unix_due = unix_us() + (due_us - now_us());
        let latency = self.latency_ms.load(Ordering::Relaxed);
        let _ = self.tx.send(Self::frame(AUDIO, latency, unix_due, pcm));
    }

    fn flush(&self) {
        let _ = self.tx.send(Self::frame(FLUSH, 0, 0, &[]));
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn clean_info(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_' | ','))
        .take(MAX_INFO)
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn clean_kind(raw: &str) -> &'static str {
    match raw {
        "desktop" => "desktop",
        "phone" => "phone",
        "tablet" => "tablet",
        _ => "browser",
    }
}

pub fn label(id: &str) -> &str {
    id.rsplit_once('-').map_or(id, |(label, _)| label)
}

pub fn tag(id: &str) -> &str {
    id.rsplit_once('-').map_or("", |(_, tag)| tag)
}

pub fn device(id: &str) -> Device {
    Device {
        udn: format!("{BROWSER_PREFIX}{id}"),
        name: label(id).to_string(),
        location: String::new(),
        protocol: BROWSER.into(),
    }
}

pub struct BrowserOutput {
    device: Device,
    stream: Stream,
    volume: AtomicU32,
}

impl BrowserOutput {
    pub fn connect(device: &Device, dir: &Directory) -> Result<BrowserOutput> {
        let id = device
            .udn
            .strip_prefix(BROWSER_PREFIX)
            .filter(|id| valid_id(id))
            .ok_or_else(|| anyhow!("not a browser"))?;
        let sink = BrowserSink::new(dir.pcm.sender(id), Arc::new(AtomicI32::new(LEAD_MS as i32)));
        Ok(BrowserOutput {
            device: device.clone(),
            stream: Stream::spawn("browser-engine", LEAD_MS, Arc::new(sink))?,
            volume: AtomicU32::new(u32::from(DEFAULT_VOLUME)),
        })
    }
}

impl Output for BrowserOutput {
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
        Ok(self.volume.load(Ordering::Relaxed))
    }

    fn set_volume(&self, percent: u32) -> Result<()> {
        self.volume.store(percent, Ordering::Relaxed);
        Ok(())
    }

    fn close(&self) {
        self.stream.close();
    }
}
