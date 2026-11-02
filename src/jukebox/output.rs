use std::sync::Arc;

use anyhow::Result;

use super::airplay::{self, AirplayOutput, Scanner};
use super::browser::BrowserOutput;
use super::dlna::{self, Device, DlnaOutput};
use super::group::{self, Group, GroupOutput};
use super::snapcast::{self, Hub, SnapcastOutput};

#[derive(Debug)]
pub struct Unreachable(pub String);

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreachable {}

pub fn is_unreachable(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Unreachable>().is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Playing,
    Transitioning,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Default)]
pub struct Position {
    pub song_id: Option<String>,
    pub seconds: Option<u64>,
}

pub struct Track<'a> {
    pub id: &'a str,
    pub path: &'a std::path::Path,
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    pub content_type: &'a str,
    pub duration_secs: u64,
    pub stream_path: &'a str,
    pub port: u16,
}

pub trait Output: Send + Sync {
    fn device(&self) -> &Device;
    fn load(&self, track: &Track, offset: u64) -> Result<()>;
    fn resume(&self) -> Result<()>;
    fn pause(&self) -> Result<()>;
    fn stop(&self) -> Result<()>;
    fn seek(&self, secs: u64) -> Result<()>;
    fn state(&self) -> Result<State>;
    fn position(&self) -> Result<Position>;
    fn volume(&self) -> Result<u32>;
    fn set_volume(&self, percent: u32) -> Result<()>;
    fn set_delays(&self, _delays: &[(String, i32)]) -> Result<()> {
        Ok(())
    }
    fn clock(&self) -> Option<super::stream::Clock> {
        None
    }
    fn set_members(&self, _group: &Group, _dir: &Directory) -> Result<bool> {
        Ok(false)
    }
    fn close(&self) {}
}

pub type SharedOutput = Arc<dyn Output>;

#[derive(Clone)]
pub struct Directory {
    pub hub: Arc<Hub>,
    pub pcm: Arc<super::browser::Pcm>,
    pub airplay: Arc<Scanner>,
    pub known: Vec<Device>,
    pub groups: Vec<Group>,
}

pub fn connect(location: &str) -> Result<SharedOutput> {
    Ok(Arc::new(DlnaOutput::connect(location)?))
}

pub fn open(device: &Device, dir: &Directory) -> Result<SharedOutput> {
    match device.protocol.as_str() {
        snapcast::PROTOCOL => Ok(Arc::new(SnapcastOutput::connect(&dir.hub, device)?)),
        group::BROWSER => Ok(Arc::new(BrowserOutput::connect(device, dir)?)),
        airplay::PROTOCOL => Ok(Arc::new(AirplayOutput::connect(&dir.airplay, device)?)),
        group::PROTOCOL => {
            let group = dir
                .groups
                .iter()
                .find(|g| g.device().udn == device.udn)
                .ok_or_else(|| anyhow::anyhow!("unknown group"))?;
            Ok(Arc::new(GroupOutput::connect(group, dir)?))
        }
        _ => connect(&device.location),
    }
}

pub fn locate(saved: &Device, dir: &Directory) -> Option<SharedOutput> {
    if matches!(
        saved.protocol.as_str(),
        snapcast::PROTOCOL | airplay::PROTOCOL | group::PROTOCOL | group::BROWSER
    ) {
        return open(saved, dir).ok();
    }
    if let Ok(o) = connect(&saved.location)
        && o.device().udn == saved.udn
    {
        return Some(o);
    }
    let found = dlna::discover().into_iter().find(|d| d.udn == saved.udn)?;
    connect(&found.location).ok()
}

pub fn discover() -> Vec<Device> {
    dlna::discover()
}
