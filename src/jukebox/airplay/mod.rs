pub mod proto;
pub mod session;

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::Duration;

use anyhow::{Result, anyhow};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use tokio::sync::watch;

use super::dlna::Device;
use super::output::{Output, Position, State, Track, Unreachable};
use super::snapcast::BUFFER_MS;
use super::stream::{SharedSink, Sink, Stream};
pub use session::{Auth, NeedsAuth, Pairing, PendingPairing};
use session::{Encryption, Kind, Sender, Target};

pub const PROTOCOL: &str = "airplay";
pub const PREFIX: &str = "airplay:";
pub const DEFAULT_VOLUME: u32 = 50;
const AIRPLAY: &str = "_airplay._tcp.local.";
const RAOP: &str = "_raop._tcp.local.";

pub fn id_of(udn: &str) -> Option<&str> {
    udn.strip_prefix(PREFIX).filter(|id| !id.is_empty())
}

#[derive(Clone, Debug, Default)]
struct Found {
    name: String,
    ap2: Option<Target>,
    raop: Option<Target>,
    needs: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lock {
    pub needs: Option<&'static str>,
    pub configured: bool,
    pub paired: bool,
}

impl Found {
    fn target(&self) -> Option<Target> {
        self.ap2.clone().or_else(|| self.raop.clone())
    }
}

pub struct Scanner {
    found: Mutex<HashMap<String, Found>>,
    auth: Mutex<HashMap<String, Auth>>,
    names: Mutex<HashMap<String, (String, bool)>>,
    daemon: Mutex<Option<ServiceDaemon>>,
    changes: watch::Sender<u64>,
}

fn txt_flag(value: Option<&str>, bit: u64) -> bool {
    value
        .and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok())
        .is_some_and(|v| v & bit != 0)
}

fn features(value: Option<&str>) -> u64 {
    let Some(value) = value else {
        return 0;
    };
    let mut parts = value
        .split(',')
        .map(|p| u64::from_str_radix(p.trim().trim_start_matches("0x"), 16).unwrap_or(0));
    let low = parts.next().unwrap_or(0);
    let high = parts.next().unwrap_or(0);
    low | (high << 32)
}

fn has(features: u64, bit: u32) -> bool {
    features & (1u64 << bit) != 0
}

fn list(value: Option<&str>) -> Vec<&str> {
    value
        .map(|v| v.split(',').map(str::trim).collect())
        .unwrap_or_default()
}

pub fn ap2_access(get: &dyn Fn(&str) -> Option<String>) -> Option<Option<&'static str>> {
    let f = features(get("features").as_deref());
    let audio = has(f, 9);
    let airplay2 = has(f, 38) || has(f, 48);
    let pairing = has(f, 46) || has(f, 48);
    let flags = get("flags").or_else(|| get("sf"));
    let pin = txt_flag(flags.as_deref(), 0x8);
    let password = txt_flag(flags.as_deref(), 0x80)
        || get("pw").is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let acl = get("acl").is_none_or(|v| v == "0");
    let act = get("act").is_none_or(|v| v != "2");
    if !(audio && airplay2 && pairing && acl && act) {
        return None;
    }
    Some(if password {
        Some("password")
    } else if pin {
        Some("pin")
    } else {
        None
    })
}

pub fn raop_encryption(get: &dyn Fn(&str) -> Option<String>) -> Option<Encryption> {
    let codecs = get("cn");
    if !list(codecs.as_deref()).contains(&"1") && codecs.is_some() {
        return None;
    }
    if get("tp").is_some_and(|tp| !tp.contains("UDP")) {
        return None;
    }
    if get("pw").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
        return None;
    }
    let et = get("et");
    let kinds = list(et.as_deref());
    match (kinds.contains(&"0") || et.is_none(), kinds.contains(&"1")) {
        (true, true) => Some(Encryption::PlainOrRsa),
        (true, false) => Some(Encryption::Plain),
        _ => None,
    }
}

fn pick(addresses: impl Iterator<Item = IpAddr>) -> Option<IpAddr> {
    let mut v4 = None;
    for address in addresses {
        match address {
            IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_link_local() => return Some(address),
            IpAddr::V4(_) => v4 = v4.or(Some(address)),
            _ => {}
        }
    }
    v4
}

impl Scanner {
    pub fn new() -> Arc<Scanner> {
        Arc::new(Scanner {
            found: Mutex::new(HashMap::new()),
            auth: Mutex::new(HashMap::new()),
            names: Mutex::new(HashMap::new()),
            daemon: Mutex::new(None),
            changes: watch::channel(0).0,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Found>> {
        self.found.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    pub fn start(self: &Arc<Self>) -> Result<()> {
        let daemon = ServiceDaemon::new()?;
        let airplay = daemon.browse(AIRPLAY)?;
        let raop = daemon.browse(RAOP)?;
        *self.daemon.lock().unwrap_or_else(|e| e.into_inner()) = Some(daemon);
        for (name, receiver, kind) in [
            ("airplay-browse", airplay, Kind::Ap2),
            ("raop-browse", raop, Kind::Raop),
        ] {
            let scanner = self.clone();
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || {
                    while let Ok(event) = receiver.recv() {
                        match event {
                            ServiceEvent::ServiceResolved(service) => {
                                let addresses =
                                    service.get_addresses().iter().map(|a| a.to_ip_addr());
                                let Some(ip) = pick(addresses) else {
                                    continue;
                                };
                                let txt = |key: &str| {
                                    service.get_property_val_str(key).map(str::to_string)
                                };
                                scanner.resolved(
                                    kind,
                                    service.get_fullname(),
                                    SocketAddr::new(ip, service.get_port()),
                                    &txt,
                                );
                            }
                            ServiceEvent::ServiceRemoved(_, fullname) => scanner.removed(&fullname),
                            _ => {}
                        }
                    }
                })?;
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        if let Some(daemon) = self.daemon.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = daemon.shutdown();
        }
    }

    pub fn resolved(
        &self,
        kind: Kind,
        fullname: &str,
        address: SocketAddr,
        txt: &dyn Fn(&str) -> Option<String>,
    ) {
        let instance = fullname.split("._").next().unwrap_or(fullname);
        let (id, name) = match kind {
            Kind::Raop => match instance.split_once('@') {
                Some((mac, name)) => (mac.replace(':', "").to_uppercase(), name.to_string()),
                None => return,
            },
            Kind::Ap2 => match txt("deviceid") {
                Some(id) => (id.replace(':', "").to_uppercase(), instance.to_string()),
                None => return,
            },
        };
        let model = txt("model").or_else(|| txt("am"));
        let access = if kind == Kind::Ap2 {
            ap2_access(txt)
        } else {
            None
        };
        let target = match kind {
            Kind::Ap2 => access.map(|_| Target {
                name: name.clone(),
                address,
                kind,
                encryption: Encryption::Plain,
                model: model.clone(),
            }),
            Kind::Raop => raop_encryption(txt).map(|encryption| Target {
                name: name.clone(),
                address,
                kind,
                encryption,
                model: model.clone(),
            }),
        };
        self.names.lock().unwrap_or_else(|e| e.into_inner()).insert(
            fullname.to_string(),
            (id.clone(), matches!(kind, Kind::Ap2)),
        );
        {
            let mut found = self.lock();
            let entry = found.entry(id).or_default();
            entry.name = name;
            match kind {
                Kind::Ap2 => {
                    entry.ap2 = target;
                    entry.needs = access.flatten();
                }
                Kind::Raop => entry.raop = target,
            }
        }
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    pub fn removed(&self, fullname: &str) {
        let Some((id, ap2)) = self
            .names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(fullname)
        else {
            return;
        };
        {
            let mut found = self.lock();
            if let Some(entry) = found.get_mut(&id) {
                if ap2 {
                    entry.ap2 = None;
                } else {
                    entry.raop = None;
                }
                if entry.target().is_none() {
                    found.remove(&id);
                }
            }
        }
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    pub fn devices(&self) -> Vec<Device> {
        let mut out: Vec<Device> = self
            .lock()
            .iter()
            .filter(|(_, found)| found.target().is_some())
            .map(|(id, found)| Device {
                udn: format!("{PREFIX}{id}"),
                name: found.name.clone(),
                location: found
                    .target()
                    .map(|t| t.address.ip().to_string())
                    .unwrap_or_default(),
                protocol: PROTOCOL.into(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn target(&self, udn: &str) -> Option<Target> {
        self.lock().get(id_of(udn)?)?.target()
    }

    fn auths(&self) -> std::sync::MutexGuard<'_, HashMap<String, Auth>> {
        self.auth.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn auth(&self, udn: &str) -> Auth {
        self.auths().get(udn).cloned().unwrap_or_default()
    }

    pub fn set_auth(&self, udn: &str, auth: Auth) {
        let mut all = self.auths();
        if auth == Auth::default() {
            all.remove(udn);
        } else {
            all.insert(udn.to_string(), auth);
        }
    }

    pub fn load_auth(&self, all: HashMap<String, Auth>) {
        *self.auths() = all;
    }

    pub fn export_auth(&self) -> HashMap<String, Auth> {
        self.auths().clone()
    }

    pub fn lock_of(&self, udn: &str) -> Option<Lock> {
        let needs = self.lock().get(id_of(udn)?)?.needs;
        let auth = self.auth(udn);
        Some(Lock {
            needs,
            configured: auth.password.is_some() || auth.pairing.is_some(),
            paired: auth.pairing.is_some(),
        })
    }

    pub fn address_of(&self, udn: &str) -> Option<SocketAddr> {
        self.target(udn).map(|t| t.address)
    }
}

pub struct Handle {
    pub udn: String,
    sender: Sender,
    volume: AtomicU32,
}

impl Sink for Handle {
    fn push(&self, due_us: i64, pcm: &[u8]) {
        self.sender.push(due_us, pcm);
    }

    fn flush(&self) {
        self.sender.flush();
    }
}

impl Handle {
    pub fn connect(scanner: &Scanner, device: &Device, latency_ms: i32) -> Result<Arc<Handle>> {
        let target = scanner
            .target(&device.udn)
            .ok_or_else(|| Unreachable(format!("{} is not on the network", device.name)))?;
        let auth = scanner.auth(&device.udn);
        let sender = Sender::open(&target, DEFAULT_VOLUME, latency_ms, &auth).map_err(|e| {
            if e.downcast_ref::<session::Refused>().is_some()
                || e.downcast_ref::<NeedsAuth>().is_some()
            {
                e
            } else {
                anyhow::Error::new(Unreachable(format!("{}: {e:#}", device.name)))
            }
        })?;
        Ok(Arc::new(Handle {
            udn: device.udn.clone(),
            sender,
            volume: AtomicU32::new(DEFAULT_VOLUME),
        }))
    }

    pub fn alive(&self) -> bool {
        self.sender.alive()
    }

    pub fn set_latency(&self, latency_ms: i32) {
        self.sender.set_latency(latency_ms);
    }

    pub fn volume(&self) -> u32 {
        self.volume.load(Ordering::Relaxed)
    }

    pub fn set_volume(&self, percent: u32) {
        self.volume.store(percent, Ordering::Relaxed);
        self.sender.set_volume(percent);
    }

    pub fn close(&self) {
        self.sender.close();
    }
}

pub struct AirplayOutput {
    device: Device,
    handle: Arc<Handle>,
    stream: Stream,
}

impl AirplayOutput {
    pub fn connect(scanner: &Scanner, device: &Device) -> Result<AirplayOutput> {
        let handle = Handle::connect(scanner, device, BUFFER_MS as i32)?;
        let sink: SharedSink = handle.clone();
        Ok(AirplayOutput {
            device: device.clone(),
            stream: Stream::spawn("airplay-engine", u64::from(BUFFER_MS), sink)?,
            handle,
        })
    }

    fn check(&self) -> Result<()> {
        if self.handle.alive() {
            Ok(())
        } else {
            Err(anyhow!(Unreachable(format!(
                "{} stopped responding",
                self.device.name
            ))))
        }
    }
}

impl Output for AirplayOutput {
    fn device(&self) -> &Device {
        &self.device
    }

    fn load(&self, track: &Track, offset: u64) -> Result<()> {
        self.check()?;
        self.stream
            .play(Some(track.id.to_string()), track.path.to_path_buf(), offset)
    }

    fn resume(&self) -> Result<()> {
        self.check()?;
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
        self.check()?;
        self.stream.seek(secs)
    }

    fn state(&self) -> Result<State> {
        self.check()?;
        Ok(self.stream.state())
    }

    fn position(&self) -> Result<Position> {
        Ok(self.stream.position())
    }

    fn volume(&self) -> Result<u32> {
        Ok(self.handle.volume())
    }

    fn set_volume(&self, percent: u32) -> Result<()> {
        self.handle.set_volume(percent);
        Ok(())
    }

    fn close(&self) {
        self.stream.close();
        self.handle.close();
    }
}

impl Drop for AirplayOutput {
    fn drop(&mut self) {
        Output::close(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn txt(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn a_mac_that_allows_anyone_is_an_airplay_2_target() {
        let get = txt(&[
            ("features", "0x4A7FCFD5,0x38174FDE"),
            ("flags", "0x4"),
            ("acl", "0"),
        ]);
        assert_eq!(ap2_access(&get), Some(None));
    }

    #[test]
    fn pin_and_password_devices_are_listed_with_what_they_need() {
        let base = [("features", "0x4A7FCFD5,0x38174FDE"), ("acl", "0")];
        let with = |extra: (&str, &str)| {
            let mut pairs = base.to_vec();
            pairs.push(extra);
            ap2_access(&txt(&pairs))
        };
        assert_eq!(with(("flags", "0xc")), Some(Some("pin")));
        assert_eq!(with(("flags", "0x284")), Some(Some("password")));
        assert_eq!(with(("pw", "true")), Some(Some("password")));
        assert_eq!(with(("act", "2")), None);
        assert_eq!(
            ap2_access(&txt(&[("features", "0x4A7FCFD5,0x38174FDE"), ("acl", "1")])),
            None
        );
        assert_eq!(ap2_access(&txt(&[("features", "0x200,0x0")])), None);
    }

    #[test]
    fn raop_needs_alac_udp_and_a_stream_we_can_encrypt() {
        let plain = txt(&[("cn", "0,1,2,3"), ("et", "0,3,5"), ("tp", "UDP")]);
        assert_eq!(raop_encryption(&plain), Some(Encryption::Plain));
        let both = txt(&[("cn", "1"), ("et", "0,1"), ("tp", "TCP,UDP")]);
        assert_eq!(raop_encryption(&both), Some(Encryption::PlainOrRsa));
        let rsa_only = txt(&[("cn", "1"), ("et", "1"), ("tp", "UDP")]);
        assert_eq!(raop_encryption(&rsa_only), None);
        let no_alac = txt(&[("cn", "0,2"), ("et", "0"), ("tp", "UDP")]);
        assert_eq!(raop_encryption(&no_alac), None);
        let password = txt(&[("cn", "1"), ("et", "0"), ("pw", "true")]);
        assert_eq!(raop_encryption(&password), None);
    }

    #[test]
    fn both_records_of_one_speaker_become_one_device() {
        let scanner = Scanner::new();
        let address = SocketAddr::from(([10, 0, 1, 9], 7000));
        scanner.resolved(
            Kind::Raop,
            "A243BE856C32@PandaMAC._raop._tcp.local.",
            address,
            &txt(&[("cn", "1"), ("et", "0"), ("tp", "UDP")]),
        );
        scanner.resolved(
            Kind::Ap2,
            "PandaMAC._airplay._tcp.local.",
            address,
            &txt(&[
                ("deviceid", "A2:43:BE:85:6C:32"),
                ("features", "0x4A7FCFD5,0x38174FDE"),
                ("acl", "0"),
            ]),
        );
        let devices = scanner.devices();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].udn, "airplay:A243BE856C32");
        assert_eq!(devices[0].name, "PandaMAC");
        assert_eq!(
            scanner.target("airplay:A243BE856C32").unwrap().kind,
            Kind::Ap2
        );
        scanner.removed("PandaMAC._airplay._tcp.local.");
        assert_eq!(
            scanner.target("airplay:A243BE856C32").unwrap().kind,
            Kind::Raop
        );
        scanner.removed("A243BE856C32@PandaMAC._raop._tcp.local.");
        assert!(scanner.devices().is_empty());
    }

    fn live_auth() -> Auth {
        Auth {
            password: std::env::var("JUUZ_AIRPLAY_PASSWORD").ok(),
            pairing: None,
        }
    }

    #[test]
    #[ignore]
    fn live_silent_session() {
        let Ok(address) = std::env::var("JUUZ_AIRPLAY_TEST") else {
            return;
        };
        let kind = if std::env::var("JUUZ_AIRPLAY_KIND").is_ok_and(|k| k == "raop") {
            Kind::Raop
        } else {
            Kind::Ap2
        };
        let target = Target {
            name: "live".into(),
            address: address.parse().unwrap(),
            kind,
            encryption: Encryption::PlainOrRsa,
            model: None,
        };
        let sender = Sender::open(&target, 0, 1000, &live_auth()).unwrap();
        let start = crate::jukebox::stream::now_us();
        let seconds: i64 = std::env::var("JUUZ_AIRPLAY_SECONDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        for n in 0..seconds * 50 {
            sender.push(start + n * 20_000, &vec![0u8; 960 * 4]);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(sender.alive());
        sender.flush();
        assert!(sender.alive());
        sender.close();
    }

    #[test]
    #[ignore]
    fn live_music_snippet() {
        let (Ok(address), Ok(file)) = (
            std::env::var("JUUZ_AIRPLAY_TEST"),
            std::env::var("JUUZ_AIRPLAY_FILE"),
        ) else {
            return;
        };
        let target = Target {
            name: "live".into(),
            address: address.parse().unwrap(),
            kind: Kind::Ap2,
            encryption: Encryption::PlainOrRsa,
            model: None,
        };
        let sender = Arc::new(Sender::open(&target, 15, 1000, &live_auth()).unwrap());
        let handle: SharedSink = Arc::new(Handle {
            udn: "live".into(),
            sender: Arc::try_unwrap(sender).ok().unwrap(),
            volume: AtomicU32::new(15),
        });
        let stream = Stream::spawn("live", 1000, handle).unwrap();
        stream
            .play(Some("live".into()), std::path::PathBuf::from(file), 30)
            .unwrap();
        std::thread::sleep(Duration::from_secs(10));
        stream.stop();
        std::thread::sleep(Duration::from_millis(500));
    }
}
