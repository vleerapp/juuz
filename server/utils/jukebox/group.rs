use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use super::airplay::{self, Handle};
use super::browser::BrowserSink;
use super::dlna::Device;
use super::output::{self, Directory, Output, Position, SharedOutput, State, Track};
pub use super::snapcast::BUFFER_MS;
use super::snapcast::{self, Hub};
use super::stream::{SharedSink, Sink, Stream};

pub const PROTOCOL: &str = "group";
pub const PREFIX: &str = "group:";
pub const BROWSER: &str = "browser";
pub const BROWSER_PREFIX: &str = "browser:";
pub const MAX_MEMBERS: usize = 16;
pub const MIN_DELAY_MS: i32 = -(BUFFER_MS as i32) + 100;
pub const MAX_DELAY_MS: i32 = 3000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Member {
    pub udn: String,
    pub name: String,
    #[serde(default)]
    pub location: String,
    pub protocol: String,
    #[serde(default)]
    pub delay_ms: i32,
}

impl Member {
    pub fn is_browser(&self) -> bool {
        self.protocol == BROWSER
    }

    pub fn browser(udn: &str, delay_ms: i32) -> Option<Member> {
        let name = udn.strip_prefix(BROWSER_PREFIX)?;
        super::browser::valid_id(name).then(|| Member {
            udn: udn.to_string(),
            name: name.to_string(),
            location: String::new(),
            protocol: BROWSER.into(),
            delay_ms: delay_ms.clamp(MIN_DELAY_MS, MAX_DELAY_MS),
        })
    }

    pub fn device(&self) -> Device {
        Device {
            udn: self.udn.clone(),
            name: self.name.clone(),
            location: self.location.clone(),
            protocol: self.protocol.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub members: Vec<Member>,
}

impl Group {
    pub fn device(&self) -> Device {
        Device {
            udn: format!("{PREFIX}{}", self.id),
            name: self.name.clone(),
            location: String::new(),
            protocol: PROTOCOL.into(),
        }
    }
}

struct Roster(RwLock<Vec<(String, SharedSink)>>);

impl Roster {
    fn read(&self) -> Vec<SharedSink> {
        let all = self.0.read().unwrap_or_else(|e| e.into_inner());
        all.iter().map(|(_, sink)| sink.clone()).collect()
    }

    fn add(&self, udn: &str, sink: SharedSink) {
        let mut all = self.0.write().unwrap_or_else(|e| e.into_inner());
        all.retain(|(u, _)| u != udn);
        all.push((udn.to_string(), sink));
    }

    fn remove(&self, udn: &str) {
        let mut all = self.0.write().unwrap_or_else(|e| e.into_inner());
        all.retain(|(u, _)| u != udn);
    }
}

impl Sink for Roster {
    fn push(&self, due_us: i64, pcm: &[u8]) {
        let all = self.0.read().unwrap_or_else(|e| e.into_inner());
        for (_, sink) in all.iter() {
            sink.push(due_us, pcm);
        }
    }

    fn flush(&self) {
        let sinks = self.read();
        match sinks.as_slice() {
            [] => {}
            [only] => only.flush(),
            many => std::thread::scope(|scope| {
                for sink in many {
                    scope.spawn(|| sink.flush());
                }
            }),
        }
    }
}

#[derive(Default)]
struct Live {
    keys: Vec<(String, String)>,
    airplay: Vec<Arc<Handle>>,
    pull: Vec<(String, SharedOutput)>,
    browsers: Vec<(String, Arc<AtomicI32>)>,
}

pub struct GroupOutput {
    device: Device,
    hub: Arc<Hub>,
    roster: Arc<Roster>,
    stream: Option<Stream>,
    live: Mutex<Live>,
    volume: AtomicU32,
}

impl GroupOutput {
    pub fn connect(group: &Group, dir: &Directory) -> Result<GroupOutput> {
        let roster = Arc::new(Roster(RwLock::new(Vec::new())));
        let mut live = Live::default();
        for member in &group.members {
            let device = dir
                .known
                .iter()
                .find(|d| d.udn == member.udn)
                .cloned()
                .unwrap_or_else(|| member.device());
            if device.protocol == snapcast::PROTOCOL {
                let Some(key) = snapcast::key_of(&device.udn) else {
                    continue;
                };
                dir.hub
                    .set_latency(key, -member.delay_ms.clamp(MIN_DELAY_MS, MAX_DELAY_MS));
                roster.add(
                    &device.udn,
                    snapcast::emitter(dir.hub.clone(), vec![key.to_string()]),
                );
                live.keys.push((device.udn.clone(), key.to_string()));
            } else if device.protocol == airplay::PROTOCOL {
                let latency = BUFFER_MS as i32 + member.delay_ms.clamp(MIN_DELAY_MS, MAX_DELAY_MS);
                match Handle::connect(&dir.airplay, &device, latency) {
                    Ok(handle) => {
                        roster.add(&device.udn, handle.clone());
                        live.airplay.push(handle);
                    }
                    Err(e) => tracing::warn!("group member {} is unreachable: {e:#}", device.name),
                }
            } else if device.protocol == BROWSER {
                let Some(id) = device.udn.strip_prefix(BROWSER_PREFIX) else {
                    continue;
                };
                let latency = Arc::new(AtomicI32::new(
                    BUFFER_MS as i32 + member.delay_ms.clamp(MIN_DELAY_MS, MAX_DELAY_MS),
                ));
                roster.add(
                    &device.udn,
                    Arc::new(BrowserSink::new(dir.pcm.sender(id), latency.clone())),
                );
                live.browsers.push((device.udn.clone(), latency));
            } else if device.protocol == PROTOCOL {
                continue;
            } else {
                match output::locate(&device, dir) {
                    Some(o) => live.pull.push((device.udn.clone(), o)),
                    None => tracing::warn!("group member {} is unreachable", device.name),
                }
            }
        }
        if live.keys.is_empty()
            && live.airplay.is_empty()
            && live.pull.is_empty()
            && live.browsers.is_empty()
        {
            return Err(anyhow!("none of the group members are available"));
        }
        let volume = live
            .keys
            .first()
            .map(|(_, key)| dir.hub.volume(key))
            .or_else(|| live.airplay.first().map(|h| h.volume()))
            .or_else(|| live.pull.first().and_then(|(_, o)| o.volume().ok()))
            .unwrap_or(u32::from(snapcast::DEFAULT_VOLUME));
        let stream = if live.keys.is_empty() && live.airplay.is_empty() && live.browsers.is_empty()
        {
            None
        } else {
            Some(Stream::spawn(
                "group-engine",
                u64::from(BUFFER_MS),
                roster.clone(),
            )?)
        };
        Ok(GroupOutput {
            device: group.device(),
            hub: dir.hub.clone(),
            roster,
            stream,
            live: Mutex::new(live),
            volume: AtomicU32::new(volume),
        })
    }

    fn live(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn pulls(&self) -> Vec<SharedOutput> {
        self.live().pull.iter().map(|(_, o)| o.clone()).collect()
    }

    fn each_pull(&self, what: &str, f: impl Fn(&dyn Output) -> Result<()> + Sync) {
        let pull = self.pulls();
        std::thread::scope(|scope| {
            for o in &pull {
                let f = &f;
                scope.spawn(move || {
                    if let Err(e) = f(&**o) {
                        tracing::warn!("group member {} {what} failed: {e:#}", o.device().name);
                    }
                });
            }
        });
    }
}

impl Output for GroupOutput {
    fn device(&self) -> &Device {
        &self.device
    }

    fn load(&self, track: &Track, offset: u64) -> Result<()> {
        let mut result = Ok(());
        let pulls = !self.live().pull.is_empty();
        std::thread::scope(|scope| {
            if pulls {
                scope.spawn(|| self.each_pull("load", |o| o.load(track, offset)));
            }
            if let Some(stream) = &self.stream {
                result = stream.play(Some(track.id.to_string()), track.path.to_path_buf(), offset);
            }
        });
        result
    }

    fn resume(&self) -> Result<()> {
        if let Some(stream) = &self.stream {
            stream.resume();
        }
        self.each_pull("resume", |o| o.resume());
        Ok(())
    }

    fn pause(&self) -> Result<()> {
        if let Some(stream) = &self.stream {
            stream.pause();
        }
        self.each_pull("pause", |o| o.pause());
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        if let Some(stream) = &self.stream {
            stream.stop();
        }
        self.each_pull("stop", |o| o.stop());
        Ok(())
    }

    fn seek(&self, secs: u64) -> Result<()> {
        let mut result = Ok(());
        let pulls = !self.live().pull.is_empty();
        std::thread::scope(|scope| {
            if pulls {
                scope.spawn(|| self.each_pull("seek", |o| o.seek(secs)));
            }
            if let Some(stream) = &self.stream {
                result = stream.seek(secs);
            }
        });
        result
    }

    fn state(&self) -> Result<State> {
        let first = self.pulls().into_iter().next();
        match (&self.stream, first) {
            (Some(stream), _) => Ok(stream.state()),
            (None, Some(o)) => o.state(),
            (None, None) => Ok(State::Stopped),
        }
    }

    fn position(&self) -> Result<Position> {
        let first = self.pulls().into_iter().next();
        match (&self.stream, first) {
            (Some(stream), _) => Ok(stream.position()),
            (None, Some(o)) => o.position(),
            (None, None) => Ok(Position::default()),
        }
    }

    fn volume(&self) -> Result<u32> {
        Ok(self.volume.load(Ordering::Relaxed))
    }

    fn set_volume(&self, percent: u32) -> Result<()> {
        self.volume.store(percent, Ordering::Relaxed);
        {
            let live = self.live();
            for (_, key) in &live.keys {
                self.hub.set_volume(key, percent);
            }
            for handle in &live.airplay {
                handle.set_volume(percent);
            }
        }
        self.each_pull("volume", |o| o.set_volume(percent));
        Ok(())
    }

    fn set_delays(&self, delays: &[(String, i32)]) -> Result<()> {
        for (udn, ms) in delays {
            let ms = (*ms).clamp(MIN_DELAY_MS, MAX_DELAY_MS);
            let live = self.live();
            if let Some((_, key)) = live.keys.iter().find(|(u, _)| u == udn) {
                self.hub.set_latency(key, -ms);
            } else if let Some(handle) = live.airplay.iter().find(|h| h.udn == *udn) {
                handle.set_latency(BUFFER_MS as i32 + ms);
            } else if let Some((_, latency)) = live.browsers.iter().find(|(u, _)| u == udn) {
                latency.store(BUFFER_MS as i32 + ms, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    fn clock(&self) -> Option<super::stream::Clock> {
        self.stream.as_ref().and_then(Stream::clock)
    }

    fn set_members(&self, group: &Group, dir: &Directory) -> Result<bool> {
        let mut want: Vec<(Device, i32)> = Vec::new();
        for member in &group.members {
            if member.protocol == PROTOCOL {
                continue;
            }
            let device = dir
                .known
                .iter()
                .find(|d| d.udn == member.udn)
                .cloned()
                .unwrap_or_else(|| member.device());
            want.push((device, member.delay_ms.clamp(MIN_DELAY_MS, MAX_DELAY_MS)));
        }
        if want.is_empty() {
            return Ok(false);
        }
        let (have_keys, have_air, have_pull, have_web) = {
            let live = self.live();
            (
                live.keys.iter().map(|(u, _)| u.clone()).collect::<Vec<_>>(),
                live.airplay
                    .iter()
                    .map(|h| h.udn.clone())
                    .collect::<Vec<_>>(),
                live.pull.iter().map(|(u, _)| u.clone()).collect::<Vec<_>>(),
                live.browsers
                    .iter()
                    .map(|(u, _)| u.clone())
                    .collect::<Vec<_>>(),
            )
        };
        let present = |udn: &str| {
            have_keys.iter().any(|u| u == udn)
                || have_air.iter().any(|u| u == udn)
                || have_pull.iter().any(|u| u == udn)
                || have_web.iter().any(|u| u == udn)
        };
        let mut joining: Vec<&(Device, i32)> = Vec::new();
        for entry in &want {
            if present(&entry.0.udn) {
                continue;
            }
            let streamed = matches!(
                entry.0.protocol.as_str(),
                snapcast::PROTOCOL | airplay::PROTOCOL | BROWSER
            );
            if !streamed || self.stream.is_none() {
                return Ok(false);
            }
            joining.push(entry);
        }
        let mut joined: Vec<(String, Arc<Handle>)> = Vec::new();
        for (device, delay) in joining
            .iter()
            .filter(|(d, _)| d.protocol == airplay::PROTOCOL)
            .map(|e| (&e.0, e.1))
        {
            match Handle::connect(&dir.airplay, device, BUFFER_MS as i32 + delay) {
                Ok(handle) => joined.push((device.udn.clone(), handle)),
                Err(e) => tracing::warn!("group member {} is unreachable: {e:#}", device.name),
            }
        }
        let volume = self.volume.load(Ordering::Relaxed);
        let mut live = self.live();
        let keeps = |udn: &str| want.iter().any(|(d, _)| d.udn == udn);
        live.keys.retain(|(udn, key)| {
            let keep = keeps(udn);
            if !keep {
                self.roster.remove(udn);
                self.hub.set_latency(key, 0);
            }
            keep
        });
        live.airplay.retain(|handle| {
            let keep = keeps(&handle.udn);
            if !keep {
                self.roster.remove(&handle.udn);
                handle.close();
            }
            keep
        });
        live.browsers.retain(|(udn, _)| {
            let keep = keeps(udn);
            if !keep {
                self.roster.remove(udn);
            }
            keep
        });
        live.pull.retain(|(udn, o)| {
            let keep = keeps(udn);
            if !keep {
                o.close();
            }
            keep
        });
        for (device, delay) in joining.iter().map(|e| (&e.0, e.1)) {
            if device.protocol != snapcast::PROTOCOL {
                continue;
            }
            let Some(key) = snapcast::key_of(&device.udn) else {
                continue;
            };
            self.hub.set_latency(key, -delay);
            self.hub.set_volume(key, volume);
            self.roster.add(
                &device.udn,
                snapcast::emitter(self.hub.clone(), vec![key.to_string()]),
            );
            live.keys.push((device.udn.clone(), key.to_string()));
        }
        for (device, delay) in joining.iter().map(|e| (&e.0, e.1)) {
            if device.protocol != BROWSER {
                continue;
            }
            let Some(id) = device.udn.strip_prefix(BROWSER_PREFIX) else {
                continue;
            };
            let latency = Arc::new(AtomicI32::new(BUFFER_MS as i32 + delay));
            self.roster.add(
                &device.udn,
                Arc::new(BrowserSink::new(dir.pcm.sender(id), latency.clone())),
            );
            live.browsers.push((device.udn.clone(), latency));
        }
        for (udn, handle) in joined {
            handle.set_volume(volume);
            self.roster.add(&udn, handle.clone());
            live.airplay.push(handle);
        }
        drop(live);
        let delays: Vec<(String, i32)> = want.iter().map(|(d, ms)| (d.udn.clone(), *ms)).collect();
        self.set_delays(&delays)?;
        Ok(true)
    }

    fn close(&self) {
        if let Some(stream) = &self.stream {
            stream.close();
        }
        let live = self.live();
        for (_, key) in &live.keys {
            self.hub.set_latency(key, 0);
        }
        for handle in &live.airplay {
            handle.close();
        }
        for (_, o) in &live.pull {
            o.close();
        }
    }
}
