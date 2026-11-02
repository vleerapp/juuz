use std::collections::{HashMap, VecDeque};
use std::io::{Cursor, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use plist::{Dictionary, Value};
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey, StaticSecret};

use super::proto::{
    AudioCipher, FrameCipher, MAX_FRAME, TAG_LEN, alac, derive, open_tagged, random, rsa, rtp,
    seal_tagged, srp, tlv,
};
use crate::jukebox::stream::now_us;

pub const RATE: u32 = 44_100;
pub const PACKET_FRAMES: usize = 352;
const CHANNELS: usize = 2;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const SETUP_TIMEOUT: Duration = Duration::from_secs(45);
const FEEDBACK_EVERY: Duration = Duration::from_secs(2);
const KEEPALIVE_EVERY: Duration = Duration::from_secs(15);
const SYNC_EVERY: u32 = 125;
const REANCHOR_US: i64 = 80_000;
const HISTORY: usize = 1024;
const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 64 * 1024;
const PLIST: &str = "application/x-apple-binary-plist";
const RAOP_USER_AGENT: &str = "iTunes/7.6.2 (Windows; N;)";
const APPLE_RAOP_USER_AGENT: &str = "AirPlay/999.0.0";
const AIRPLAY2_USER_AGENT: &str = "AirPlay/670.6.2";
const ALAC_44100_16_2: u64 = 0x40000;
const LATENCY_MIN: u64 = 11_025;
const LATENCY_MAX: u64 = 88_200;
const TRANSIENT_PIN: &str = "3939";
const TRANSIENT: u8 = 0x10;

#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn timed_out(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
            matches!(
                io.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )
        })
    })
}

fn fatal(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Refused>().is_none() && !timed_out(e)
}

fn refused(message: impl Into<String>) -> anyhow::Error {
    Refused(message.into()).into()
}

#[derive(Debug)]
pub struct NeedsAuth(pub &'static str, pub String);

impl std::fmt::Display for NeedsAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

impl std::error::Error for NeedsAuth {}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub pairing: Option<Pairing>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pairing {
    pub id: String,
    pub ltsk: String,
    pub peer_id: String,
    pub peer_ltpk: String,
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    (text.len().is_multiple_of(2)).then_some(())?;
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Ap2,
    Raop,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encryption {
    Plain,
    PlainOrRsa,
}

#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    pub address: SocketAddr,
    pub kind: Kind,
    pub encryption: Encryption,
    pub model: Option<String>,
}

struct Response {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

struct Channel {
    write: FrameCipher,
    read: FrameCipher,
}

pub struct Rtsp {
    stream: TcpStream,
    inbox: Vec<u8>,
    channel: Option<Channel>,
    sequence: u32,
    url: String,
    user_agent: &'static str,
    instance: String,
    active_remote: u32,
    session: Option<String>,
}

impl Rtsp {
    fn connect(
        address: SocketAddr,
        session_id: u32,
        instance: String,
        user_agent: &'static str,
    ) -> Result<Self> {
        let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
            .map_err(|e| anyhow!("{address}: {e}"))?;
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
        stream.set_write_timeout(Some(IO_TIMEOUT)).ok();
        let local = stream.local_addr()?.ip();
        Ok(Self {
            stream,
            inbox: Vec::new(),
            channel: None,
            sequence: 0,
            url: format!("rtsp://{local}/{session_id}"),
            user_agent,
            instance,
            active_remote: session_id,
            session: None,
        })
    }

    fn local_ip(&self) -> Result<IpAddr> {
        Ok(self.stream.local_addr()?.ip())
    }

    fn peer_ip(&self) -> Result<IpAddr> {
        Ok(self.stream.peer_addr()?.ip())
    }

    fn set_timeout(&self, timeout: Duration) {
        self.stream.set_read_timeout(Some(timeout)).ok();
        self.stream.set_write_timeout(Some(timeout)).ok();
    }

    fn encrypt(&mut self, write_key: &[u8; 32], read_key: &[u8; 32]) {
        self.inbox.clear();
        self.channel = Some(Channel {
            write: FrameCipher::new(write_key),
            read: FrameCipher::new(read_key),
        });
    }

    fn request(
        &mut self,
        method: &str,
        target: Option<&str>,
        headers: &[(&str, String)],
        body: Option<(&str, &[u8])>,
    ) -> Result<Response> {
        self.sequence += 1;
        let mut request = format!(
            "{method} {} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: {}\r\nClient-Instance: {}\r\nDACP-ID: {}\r\nActive-Remote: {}\r\n",
            target.unwrap_or(&self.url),
            self.sequence,
            self.user_agent,
            self.instance,
            self.instance,
            self.active_remote,
        );
        if let Some(session) = &self.session {
            request.push_str(&format!("Session: {session}\r\n"));
        }
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        match body {
            Some((kind, bytes)) => request.push_str(&format!(
                "Content-Type: {kind}\r\nContent-Length: {}\r\n",
                bytes.len()
            )),
            None if self.channel.is_some() => request.push_str("Content-Length: 0\r\n"),
            None => {}
        }
        request.push_str("\r\n");
        let mut bytes = request.into_bytes();
        if let Some((_, body)) = body {
            bytes.extend_from_slice(body);
        }
        if let Some(channel) = &mut self.channel {
            bytes = channel.write.seal(&bytes);
        }
        self.stream
            .write_all(&bytes)
            .map_err(|e| anyhow::Error::new(e).context(method.to_string()))?;
        let response = self
            .read_response()
            .map_err(|e| e.context(method.to_string()))?;
        match response.status {
            200..=299 => Ok(response),
            status => Err(refused(format!("{method} answered {status}"))),
        }
    }

    fn read_response(&mut self) -> Result<Response> {
        loop {
            if let Some(message) = take_message(&mut self.inbox)? {
                let status = parse_status(&message.first_line)?;
                return Ok(Response {
                    status,
                    headers: message.headers,
                    body: message.body,
                });
            }
            self.fill()?;
        }
    }

    fn fill(&mut self) -> Result<()> {
        match &mut self.channel {
            None => {
                let mut buffer = [0u8; 4096];
                let read = self.stream.read(&mut buffer)?;
                if read == 0 {
                    bail!("the device closed the connection");
                }
                self.inbox.extend_from_slice(&buffer[..read]);
            }
            Some(channel) => {
                let mut length = [0u8; 2];
                self.stream.read_exact(&mut length)?;
                let size = usize::from(u16::from_le_bytes(length));
                if size > MAX_FRAME {
                    bail!("a {size}-byte encrypted frame");
                }
                let mut sealed = vec![0u8; size + TAG_LEN];
                self.stream.read_exact(&mut sealed)?;
                let plain = channel
                    .read
                    .open(length, &sealed)
                    .ok_or_else(|| anyhow!("an answer failed to decrypt"))?;
                self.inbox.extend_from_slice(&plain);
            }
        }
        Ok(())
    }

    fn shutdown(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

struct Message {
    first_line: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn head_end(inbox: &[u8]) -> Option<(usize, usize)> {
    (0..inbox.len()).find_map(|at| {
        let rest = &inbox[at..];
        if rest.starts_with(b"\r\n\r\n") {
            Some((at, 4))
        } else if rest.starts_with(b"\n\n") {
            Some((at, 2))
        } else {
            None
        }
    })
}

fn take_message(inbox: &mut Vec<u8>) -> Result<Option<Message>> {
    let Some((end, separator)) = head_end(inbox) else {
        if inbox.len() > MAX_HEAD {
            bail!("a message head is too large");
        }
        return Ok(None);
    };
    let head = String::from_utf8_lossy(&inbox[..end]).into_owned();
    let mut lines = head.split('\n').map(|line| line.trim_end_matches('\r'));
    let first_line = lines.next().unwrap_or_default().to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let length = headers
        .get("content-length")
        .and_then(|length| length.parse::<usize>().ok())
        .unwrap_or(0);
    if length > MAX_BODY {
        bail!("a {length}-byte message is too large");
    }
    let start = end + separator;
    if inbox.len() < start + length {
        return Ok(None);
    }
    let body = inbox[start..start + length].to_vec();
    inbox.drain(..start + length);
    Ok(Some(Message {
        first_line,
        headers,
        body,
    }))
}

fn parse_status(line: &str) -> Result<u16> {
    let mut parts = line.split_whitespace();
    let protocol = parts.next().unwrap_or_default();
    if !protocol.starts_with("RTSP/") && !protocol.starts_with("HTTP/") {
        bail!("not an RTSP answer: {}", line.trim());
    }
    parts
        .next()
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| anyhow!("not an RTSP answer: {}", line.trim()))
}

fn transport_ports(transport: &str) -> HashMap<String, u16> {
    transport
        .split(';')
        .filter_map(|part| part.split_once('='))
        .filter_map(|(name, value)| {
            value
                .trim()
                .parse::<u16>()
                .ok()
                .map(|port| (name.trim().to_ascii_lowercase(), port))
        })
        .collect()
}

struct TimingServer {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TimingServer {
    fn start(local: IpAddr) -> Result<Self> {
        let socket = UdpSocket::bind(SocketAddr::new(local, 0))?;
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .ok();
        let port = socket.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::Builder::new()
            .name("airplay-timing".into())
            .spawn(move || {
                let mut buffer = [0u8; 128];
                while !stopped.load(Ordering::Acquire) {
                    if let Ok((len, from)) = socket.recv_from(&mut buffer) {
                        let received = rtp::ntp_now();
                        if let Some(reply) = rtp::timing_reply(&buffer[..len], received) {
                            let _ = socket.send_to(&reply, from);
                        }
                    }
                }
            })?;
        Ok(Self {
            port,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for TimingServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn instance() -> [u8; 8] {
    static INSTANCE: OnceLock<[u8; 8]> = OnceLock::new();
    *INSTANCE.get_or_init(|| {
        let mut id = random::<8>();
        id[0] |= 0x10;
        id
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

fn colon_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn uuid() -> String {
    let bytes = hex(&random::<16>());
    format!(
        "{}-{}-{}-{}-{}",
        &bytes[..8],
        &bytes[8..12],
        &bytes[12..16],
        &bytes[16..20],
        &bytes[20..]
    )
}

fn session_id() -> u32 {
    u32::from_be_bytes(random::<4>()) & 0x7fff_ffff
}

pub fn volume_db(percent: u32) -> f32 {
    if percent == 0 {
        -144.0
    } else {
        -30.0 + 30.0 * (percent.min(100) as f32 / 100.0)
    }
}

pub fn percent_from_db(db: f32) -> u32 {
    if db <= -30.0 {
        0
    } else {
        (((db.min(0.0) + 30.0) / 30.0) * 100.0).round() as u32
    }
}

fn send_volume(rtsp: &mut Rtsp, percent: u32) -> Result<()> {
    let body = format!("volume: {:.6}\r\n", volume_db(percent));
    rtsp.request(
        "SET_PARAMETER",
        None,
        &[],
        Some(("text/parameters", body.as_bytes())),
    )
    .map(|_| ())
}

struct Keys {
    write: [u8; 32],
    read: [u8; 32],
    events_write: [u8; 32],
    events_read: [u8; 32],
    audio: [u8; 32],
}

fn tlv_post(rtsp: &mut Rtsp, path: &str, hkp: &str, body: &[u8], step: &str) -> Result<tlv::Items> {
    let response = rtsp
        .request(
            "POST",
            Some(path),
            &[("X-Apple-HKP", hkp.to_string())],
            Some(("application/octet-stream", body)),
        )
        .map_err(|e| {
            if e.to_string().ends_with(" 470") {
                anyhow::Error::new(NeedsAuth("pin", "the device wants a pairing code".into()))
            } else {
                e
            }
        })?;
    let items = tlv::decode(&response.body)
        .ok_or_else(|| refused(format!("pairing {step}: a malformed answer")))?;
    if let Some(code) = items.get(tlv::ERROR) {
        let code = code.first().copied().unwrap_or_default();
        return Err(if code == 2 {
            anyhow::Error::new(NeedsAuth(
                "password",
                "the password or code was refused".into(),
            ))
        } else {
            refused(format!("pairing {step} was refused (error {code})"))
        });
    }
    Ok(items)
}

fn expect_state(items: &tlv::Items, state: u8, step: &str) -> Result<()> {
    match items.get(tlv::STATE) {
        Some([found]) if *found == state => Ok(()),
        _ => Err(refused(format!("pairing {step}: an unexpected answer"))),
    }
}

fn transient_pairing(rtsp: &mut Rtsp, secret: &str) -> Result<Keys> {
    let start = tlv::encode(&[
        (tlv::METHOD, &[0]),
        (tlv::STATE, &[1]),
        (tlv::FLAGS, &[TRANSIENT]),
    ]);
    let challenge = tlv_post(rtsp, "/pair-setup", "4", &start, "M1")?;
    expect_state(&challenge, 2, "M2")?;
    let invalid = || refused("pairing M2: no salt or key");
    let salt = challenge.get(tlv::SALT).ok_or_else(invalid)?;
    let server_public = challenge.get(tlv::PUBLIC_KEY).ok_or_else(invalid)?;
    let exchange = srp::exchange("Pair-Setup", secret, salt, server_public, &random::<32>())
        .ok_or_else(|| refused("pairing M2: the device's key is invalid"))?;
    let proof = tlv::encode(&[
        (tlv::STATE, &[3]),
        (tlv::PUBLIC_KEY, &exchange.public),
        (tlv::PROOF, &exchange.proof),
    ]);
    let verified = tlv_post(rtsp, "/pair-setup", "4", &proof, "M3")?;
    expect_state(&verified, 4, "M4")?;
    if verified.get(tlv::PROOF) != Some(&exchange.server_proof[..]) {
        return Err(refused("pairing M4: the device did not prove the same key"));
    }
    let mut audio = [0u8; 32];
    audio.copy_from_slice(&exchange.key[..32]);
    Ok(session_keys(&exchange.key, audio))
}

fn session_keys(secret: &[u8], audio: [u8; 32]) -> Keys {
    Keys {
        write: derive(secret, "Control-Salt", "Control-Write-Encryption-Key"),
        read: derive(secret, "Control-Salt", "Control-Read-Encryption-Key"),
        events_write: derive(secret, "Events-Salt", "Events-Read-Encryption-Key"),
        events_read: derive(secret, "Events-Salt", "Events-Write-Encryption-Key"),
        audio,
    }
}

fn pair_verify(rtsp: &mut Rtsp, pairing: &Pairing) -> Result<Keys> {
    let bad = || refused("the stored pairing is damaged, pair the device again");
    let ltsk: [u8; 32] = unhex(&pairing.ltsk)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(bad)?;
    let peer: [u8; 32] = unhex(&pairing.peer_ltpk)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(bad)?;
    let signing = SigningKey::from_bytes(&ltsk);
    let peer = VerifyingKey::from_bytes(&peer).map_err(|_| bad())?;
    let secret = StaticSecret::from(random::<32>());
    let public = PublicKey::from(&secret);
    let first = tlv::encode(&[(tlv::STATE, &[1]), (tlv::PUBLIC_KEY, public.as_bytes())]);
    let reply = tlv_post(rtsp, "/pair-verify", "3", &first, "verify M1").map_err(|e| {
        match e.downcast_ref::<NeedsAuth>() {
            Some(_) => anyhow::Error::new(NeedsAuth(
                "pin",
                "the pairing was rejected, pair the device again".into(),
            )),
            None => e,
        }
    })?;
    expect_state(&reply, 2, "verify M2")?;
    let server_public: [u8; 32] = reply
        .get(tlv::PUBLIC_KEY)
        .and_then(|k| k.try_into().ok())
        .ok_or_else(|| refused("pairing verify M2: no key"))?;
    let shared = secret.diffie_hellman(&PublicKey::from(server_public));
    let key = derive(
        shared.as_bytes(),
        "Pair-Verify-Encrypt-Salt",
        "Pair-Verify-Encrypt-Info",
    );
    let sealed = reply
        .get(tlv::ENCRYPTED)
        .ok_or_else(|| refused("pairing verify M2: nothing to verify"))?;
    let opened = open_tagged(&key, b"PV-Msg02", sealed)
        .and_then(|plain| tlv::decode(&plain))
        .ok_or_else(|| refused("pairing verify M2: could not be opened"))?;
    let identifier = opened.get(tlv::IDENTIFIER).unwrap_or_default();
    let signature = opened
        .get(tlv::SIGNATURE)
        .and_then(|sig| Signature::from_slice(sig).ok())
        .ok_or_else(|| refused("pairing verify M2: no signature"))?;
    let mut signed = server_public.to_vec();
    signed.extend_from_slice(identifier);
    signed.extend_from_slice(public.as_bytes());
    peer.verify(&signed, &signature)
        .map_err(|_| refused("pairing verify: the device is not the one we paired with"))?;
    let mine = pairing.id.as_bytes();
    let mut message = public.as_bytes().to_vec();
    message.extend_from_slice(mine);
    message.extend_from_slice(&server_public);
    let proof = signing.sign(&message);
    let inner = tlv::encode(&[(tlv::IDENTIFIER, mine), (tlv::SIGNATURE, &proof.to_bytes())]);
    let sealed = seal_tagged(&key, b"PV-Msg03", &inner);
    let last = tlv::encode(&[(tlv::STATE, &[3]), (tlv::ENCRYPTED, &sealed)]);
    let done = tlv_post(rtsp, "/pair-verify", "3", &last, "verify M3")?;
    expect_state(&done, 4, "verify M4")?;
    Ok(session_keys(shared.as_bytes(), random::<32>()))
}

pub struct PendingPairing {
    rtsp: Rtsp,
    salt: Vec<u8>,
    server_public: Vec<u8>,
}

pub fn begin_pairing(address: SocketAddr) -> Result<PendingPairing> {
    let mut rtsp = Rtsp::connect(address, session_id(), hex(&instance()), AIRPLAY2_USER_AGENT)?;
    rtsp.request("GET", Some("/info"), &[], None)?;
    rtsp.request(
        "POST",
        Some("/pair-pin-start"),
        &[("X-Apple-HKP", "3".to_string())],
        None,
    )?;
    let start = tlv::encode(&[(tlv::METHOD, &[0]), (tlv::STATE, &[1])]);
    let challenge = tlv_post(&mut rtsp, "/pair-setup", "3", &start, "M1")?;
    expect_state(&challenge, 2, "M2")?;
    let invalid = || refused("pairing M2: no salt or key");
    Ok(PendingPairing {
        salt: challenge.get(tlv::SALT).ok_or_else(invalid)?.to_vec(),
        server_public: challenge.get(tlv::PUBLIC_KEY).ok_or_else(invalid)?.to_vec(),
        rtsp,
    })
}

pub fn finish_pairing(mut pending: PendingPairing, pin: &str) -> Result<Pairing> {
    let rtsp = &mut pending.rtsp;
    let exchange = srp::exchange(
        "Pair-Setup",
        pin.trim(),
        &pending.salt,
        &pending.server_public,
        &random::<32>(),
    )
    .ok_or_else(|| refused("pairing M2: the device's key is invalid"))?;
    let proof = tlv::encode(&[
        (tlv::STATE, &[3]),
        (tlv::PUBLIC_KEY, &exchange.public),
        (tlv::PROOF, &exchange.proof),
    ]);
    let verified = tlv_post(rtsp, "/pair-setup", "3", &proof, "M3").map_err(|e| match e
        .downcast_ref::<NeedsAuth>()
    {
        Some(_) => refused("that code was not accepted"),
        None => e,
    })?;
    expect_state(&verified, 4, "M4")?;
    if verified.get(tlv::PROOF) != Some(&exchange.server_proof[..]) {
        return Err(refused("pairing M4: the device did not prove the same key"));
    }
    let signing = SigningKey::from_bytes(&random::<32>());
    let ltpk = signing.verifying_key().to_bytes();
    let id = uuid();
    let controller = derive(
        &exchange.key,
        "Pair-Setup-Controller-Sign-Salt",
        "Pair-Setup-Controller-Sign-Info",
    );
    let mut message = controller.to_vec();
    message.extend_from_slice(id.as_bytes());
    message.extend_from_slice(&ltpk);
    let signature = signing.sign(&message);
    let key = derive(
        &exchange.key,
        "Pair-Setup-Encrypt-Salt",
        "Pair-Setup-Encrypt-Info",
    );
    let inner = tlv::encode(&[
        (tlv::IDENTIFIER, id.as_bytes()),
        (tlv::PUBLIC_KEY, &ltpk),
        (tlv::SIGNATURE, &signature.to_bytes()),
    ]);
    let sealed = seal_tagged(&key, b"PS-Msg05", &inner);
    let last = tlv::encode(&[(tlv::STATE, &[5]), (tlv::ENCRYPTED, &sealed)]);
    let reply = tlv_post(rtsp, "/pair-setup", "3", &last, "M5")?;
    expect_state(&reply, 6, "M6")?;
    let opened = reply
        .get(tlv::ENCRYPTED)
        .and_then(|sealed| open_tagged(&key, b"PS-Msg06", sealed))
        .and_then(|plain| tlv::decode(&plain))
        .ok_or_else(|| refused("pairing M6: could not be opened"))?;
    let peer_id = opened.get(tlv::IDENTIFIER).unwrap_or_default();
    let peer_ltpk = opened
        .get(tlv::PUBLIC_KEY)
        .ok_or_else(|| refused("pairing M6: no device key"))?;
    Ok(Pairing {
        id,
        ltsk: hex_lower(&signing.to_bytes()),
        peer_id: String::from_utf8_lossy(peer_id).into_owned(),
        peer_ltpk: hex_lower(peer_ltpk),
    })
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn integer(value: u64) -> Value {
    Value::Integer(value.into())
}

fn dictionary(entries: Vec<(&str, Value)>) -> Value {
    Value::Dictionary(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

fn setup(rtsp: &mut Rtsp, body: &Value) -> Result<Dictionary> {
    let mut bytes = Vec::new();
    body.to_writer_binary(&mut bytes)
        .map_err(|e| anyhow!("SETUP: {e}"))?;
    let answer = rtsp.request("SETUP", None, &[], Some((PLIST, &bytes)))?;
    if answer.body.is_empty() {
        return Ok(Dictionary::new());
    }
    Value::from_reader(Cursor::new(answer.body))
        .ok()
        .and_then(Value::into_dictionary)
        .ok_or_else(|| refused("SETUP answered something that is not a plist"))
}

fn port_in(answer: &Dictionary, key: &str) -> Option<u16> {
    answer
        .get(key)
        .and_then(Value::as_unsigned_integer)
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)
}

fn session_setup(timing_port: u16) -> Value {
    dictionary(vec![
        ("deviceID", Value::String(colon_hex(&instance()))),
        ("sessionUUID", Value::String(uuid())),
        ("timingPort", integer(timing_port.into())),
        ("timingProtocol", Value::String("NTP".into())),
    ])
}

fn stream_setup(session_id: u32, key: &[u8; 32], data_port: u16, control_port: u16) -> Value {
    let stream = dictionary(vec![
        ("audioFormat", integer(ALAC_44100_16_2)),
        ("audioMode", Value::String("default".into())),
        ("controlPort", integer(control_port.into())),
        ("ct", integer(2)),
        ("dataPort", integer(data_port.into())),
        ("isMedia", Value::Boolean(true)),
        ("latencyMax", integer(LATENCY_MAX)),
        ("latencyMin", integer(LATENCY_MIN)),
        ("shk", Value::Data(key.to_vec())),
        ("spf", integer(PACKET_FRAMES as u64)),
        ("sr", integer(RATE.into())),
        ("streamConnectionID", integer(session_id.into())),
        ("supportsDynamicStreamID", Value::Boolean(false)),
        ("type", integer(96)),
    ]);
    dictionary(vec![("streams", Value::Array(vec![stream]))])
}

struct Link {
    rtsp: Rtsp,
    audio: UdpSocket,
    control: UdpSocket,
    timing: TimingServer,
    control_target: SocketAddr,
    ssrc: u32,
    base_seq: u16,
    base_ts: u32,
    cipher: AudioCipher,
    events: Option<(TcpStream, FrameCipher, FrameCipher)>,
    raop: bool,
    receiver_frames: u32,
}

fn announce(rtsp: &mut Rtsp, sdp: &str, encrypted: bool) -> Result<Option<AudioCipher>> {
    let (sdp, cipher) = if encrypted {
        let (key, iv) = (random::<16>(), random::<16>());
        let sdp = format!(
            "{sdp}a=rsaaeskey:{}\r\na=aesiv:{}\r\n",
            STANDARD_NO_PAD.encode(rsa::encrypt_for_airport(&key)),
            STANDARD_NO_PAD.encode(iv)
        );
        (sdp, Some(AudioCipher::aes_cbc(key, iv)))
    } else {
        (sdp.to_string(), None)
    };
    rtsp.request(
        "ANNOUNCE",
        None,
        &[],
        Some(("application/sdp", sdp.as_bytes())),
    )?;
    Ok(cipher)
}

fn raop_user_agent(model: Option<&str>) -> &'static str {
    let apple = model.is_some_and(|model| {
        let model = model.to_ascii_lowercase();
        model.contains("audioaccessory") || model.contains("appletv")
    });
    if apple {
        APPLE_RAOP_USER_AGENT
    } else {
        RAOP_USER_AGENT
    }
}

fn raop(target: &Target, percent: u32) -> Result<Link> {
    let session_id = session_id();
    let mut rtsp = Rtsp::connect(
        target.address,
        session_id,
        hex(&instance()),
        raop_user_agent(target.model.as_deref()),
    )?;
    let local = rtsp.local_ip()?;
    let peer = rtsp.peer_ip()?;
    let control = UdpSocket::bind(SocketAddr::new(local, 0))?;
    let timing = TimingServer::start(local)?;
    let audio = UdpSocket::bind(SocketAddr::new(local, 0))?;

    rtsp.request("OPTIONS", Some("*"), &[], None)?;
    let sdp = format!(
        "v=0\r\no=iTunes {session_id} 0 IN IP4 {local}\r\ns=iTunes\r\nc=IN IP4 {peer}\r\nt=0 0\r\n\
m=audio 0 RTP/AVP 96\r\na=rtpmap:96 AppleLossless\r\n\
a=fmtp:96 {PACKET_FRAMES} 0 16 40 10 14 {CHANNELS} 255 0 0 {RATE}\r\n"
    );
    let cipher = match target.encryption {
        Encryption::Plain => announce(&mut rtsp, &sdp, false)?,
        Encryption::PlainOrRsa => match announce(&mut rtsp, &sdp, false) {
            Err(e) if e.downcast_ref::<Refused>().is_some() => {
                tracing::info!(
                    "airplay: {} refused an unencrypted stream, announcing it encrypted",
                    target.name
                );
                announce(&mut rtsp, &sdp, true)?
            }
            other => other?,
        },
    };
    let transport = format!(
        "RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;control_port={};timing_port={}",
        control.local_addr()?.port(),
        timing.port
    );
    let setup = rtsp.request("SETUP", None, &[("Transport", transport)], None)?;
    let ports = transport_ports(setup.header("Transport").unwrap_or_default());
    let server_port = *ports
        .get("server_port")
        .ok_or_else(|| refused("SETUP did not name an audio port"))?;
    let remote_control = match ports.get("control_port") {
        Some(port) => *port,
        None => server_port
            .checked_add(1)
            .ok_or_else(|| refused("SETUP named no control port"))?,
    };
    let session = setup
        .header("Session")
        .map(|session| {
            session
                .split(';')
                .next()
                .unwrap_or(session)
                .trim()
                .to_string()
        })
        .unwrap_or_else(|| "1".to_string());
    rtsp.session = Some(session);
    let base_seq = u16::from_be_bytes(random::<2>());
    let base_ts = u32::from_be_bytes(random::<4>());
    rtsp.request(
        "RECORD",
        None,
        &[
            ("Range", "npt=0-".to_string()),
            ("RTP-Info", format!("seq={base_seq};rtptime={base_ts}")),
        ],
        None,
    )?;
    send_volume(&mut rtsp, percent)?;
    audio.connect(SocketAddr::new(peer, server_port))?;
    Ok(Link {
        rtsp,
        audio,
        control,
        timing,
        control_target: SocketAddr::new(peer, remote_control),
        ssrc: u32::from_be_bytes(random::<4>()),
        base_seq,
        base_ts,
        cipher: cipher.unwrap_or(AudioCipher::None),
        events: None,
        raop: true,
        receiver_frames: 0,
    })
}

fn airplay2(target: &Target, percent: u32, auth: &Auth) -> Result<Link> {
    let session_id = session_id();
    let mut rtsp = Rtsp::connect(
        target.address,
        session_id,
        hex(&instance()),
        AIRPLAY2_USER_AGENT,
    )?;
    let local = rtsp.local_ip()?;
    let peer = rtsp.peer_ip()?;
    rtsp.request("GET", Some("/info"), &[], None)?;
    let keys = match &auth.pairing {
        Some(pairing) => pair_verify(&mut rtsp, pairing)?,
        None => transient_pairing(&mut rtsp, auth.password.as_deref().unwrap_or(TRANSIENT_PIN))
            .map_err(|e| match e.downcast_ref::<NeedsAuth>() {
                Some(needs) if needs.0 == "password" && auth.password.is_none() => e,
                Some(_) if auth.password.is_some() => anyhow::Error::new(NeedsAuth(
                    "password",
                    "the password was not accepted".into(),
                )),
                _ => e,
            })?,
    };
    rtsp.encrypt(&keys.write, &keys.read);
    let control = UdpSocket::bind(SocketAddr::new(local, 0))?;
    let timing = TimingServer::start(local)?;
    let audio = UdpSocket::bind(SocketAddr::new(local, 0))?;

    rtsp.set_timeout(SETUP_TIMEOUT);
    let session = setup(&mut rtsp, &session_setup(timing.port))
        .map_err(|e| anyhow!("session setup: {e:#}"))?;
    let events = port_in(&session, "eventPort").and_then(|events_port| {
        TcpStream::connect_timeout(&SocketAddr::new(peer, events_port), Duration::from_secs(3))
            .inspect_err(|e| tracing::info!("airplay: {} has no event channel: {e}", target.name))
            .ok()
            .map(|stream| {
                (
                    stream,
                    FrameCipher::new(&keys.events_read),
                    FrameCipher::new(&keys.events_write),
                )
            })
    });
    rtsp.request("RECORD", None, &[], None)
        .map_err(|e| anyhow!("record: {e:#}"))?;
    let stream = setup(
        &mut rtsp,
        &stream_setup(
            session_id,
            &keys.audio,
            audio.local_addr()?.port(),
            control.local_addr()?.port(),
        ),
    )
    .map_err(|e| anyhow!("stream setup: {e:#}"))?;
    let described = stream
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| streams.first())
        .and_then(Value::as_dictionary)
        .ok_or_else(|| refused("SETUP did not describe the stream"))?;
    let data_port = port_in(described, "dataPort")
        .ok_or_else(|| refused("SETUP did not name an audio port"))?;
    let remote_control = port_in(described, "controlPort")
        .ok_or_else(|| refused("SETUP did not name a control port"))?;
    let render_ms = described
        .get("arrivalToRenderLatencyMs")
        .and_then(Value::as_unsigned_integer)
        .unwrap_or(0)
        .min(1000);
    let receiver_frames = LATENCY_MIN as u32 + (render_ms * u64::from(RATE) / 1000) as u32;
    send_volume(&mut rtsp, percent)?;
    rtsp.set_timeout(IO_TIMEOUT);
    audio.connect(SocketAddr::new(peer, data_port))?;
    Ok(Link {
        rtsp,
        audio,
        control,
        timing,
        control_target: SocketAddr::new(peer, remote_control),
        ssrc: session_id,
        base_seq: u16::from_be_bytes(random::<2>()),
        base_ts: u32::from_be_bytes(random::<4>()),
        cipher: AudioCipher::new(&keys.audio),
        events,
        raop: false,
        receiver_frames,
    })
}

const TAPS: usize = 32;
const HALF: i64 = (TAPS / 2) as i64;
const IN_STEP: u64 = 160;
const PHASES: u64 = 147;

impl Default for Downsampler {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Downsampler {
    table: Vec<[f32; TAPS]>,
    hist: Vec<[f32; 2]>,
    base: i64,
    produced: u64,
}

impl Downsampler {
    pub fn new() -> Self {
        let cutoff = 0.45f64;
        let mut table = Vec::with_capacity(PHASES as usize);
        for phase in 0..PHASES {
            let frac = phase as f64 / PHASES as f64;
            let mut row = [0f32; TAPS];
            let mut sum = 0f64;
            for (m, slot) in row.iter_mut().enumerate() {
                let k = m as i64 - (HALF - 1);
                let tau = k as f64 - frac;
                let x = 2.0 * cutoff * tau;
                let sinc = if x.abs() < 1e-9 {
                    1.0
                } else {
                    (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
                };
                let w = tau / HALF as f64;
                let window = if w.abs() >= 1.0 {
                    0.0
                } else {
                    0.42 + 0.5 * (std::f64::consts::PI * w).cos()
                        + 0.08 * (2.0 * std::f64::consts::PI * w).cos()
                };
                let value = 2.0 * cutoff * sinc * window;
                sum += value;
                *slot = value as f32;
            }
            for slot in &mut row {
                *slot /= sum as f32;
            }
            table.push(row);
        }
        let mut down = Self {
            table,
            hist: Vec::new(),
            base: 0,
            produced: 0,
        };
        down.reset();
        down
    }

    pub fn reset(&mut self) {
        self.hist = vec![[0.0; 2]; (HALF - 1) as usize];
        self.base = -(HALF - 1);
        self.produced = 0;
    }

    pub fn process(&mut self, input: &[i16], out: &mut Vec<i16>) {
        for pair in input.as_chunks::<2>().0 {
            self.hist.push([f32::from(pair[0]), f32::from(pair[1])]);
        }
        loop {
            let t = (self.produced * IN_STEP / PHASES) as i64;
            let phase = ((self.produced * IN_STEP) % PHASES) as usize;
            if t + HALF >= self.base + self.hist.len() as i64 {
                break;
            }
            let start = (t - (HALF - 1) - self.base) as usize;
            let row = &self.table[phase];
            let mut left = 0f32;
            let mut right = 0f32;
            for (m, weight) in row.iter().enumerate() {
                let frame = &self.hist[start + m];
                left += frame[0] * weight;
                right += frame[1] * weight;
            }
            out.push(left.round().clamp(-32768.0, 32767.0) as i16);
            out.push(right.round().clamp(-32768.0, 32767.0) as i16);
            self.produced += 1;
        }
        let t = (self.produced * IN_STEP / PHASES) as i64;
        let drop = (t - (HALF - 1) - self.base).max(0) as usize;
        if drop > 0 {
            self.hist.drain(..drop.min(self.hist.len()));
            self.base += drop as i64;
        }
    }
}

struct Anchor {
    due_us: i64,
    ts: u32,
}

struct StreamState {
    down: Downsampler,
    input: Vec<i16>,
    pending: Vec<i16>,
    seq: u16,
    ts: u32,
    anchor: Option<Anchor>,
    first: bool,
    since_sync: u32,
}

struct Shared {
    rtsp: Mutex<Rtsp>,
    audio: UdpSocket,
    control: UdpSocket,
    control_target: SocketAddr,
    cipher: AudioCipher,
    ssrc: u32,
    raop: bool,
    latency_frames: AtomicU32,
    receiver_frames: u32,
    state: Mutex<StreamState>,
    history: Mutex<VecDeque<(u16, Vec<u8>)>>,
    stop: AtomicBool,
    alive: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Sender {
    shared: Arc<Shared>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    _timing: TimingServer,
    device_volume: u32,
}

pub fn sync_latency(latency_ms: i32, receiver_frames: u32) -> u32 {
    latency_frames(latency_ms)
        .saturating_sub(receiver_frames)
        .max(LATENCY_MIN as u32)
}

pub fn latency_frames(latency_ms: i32) -> u32 {
    let frames = i64::from(latency_ms) * i64::from(RATE) / 1000;
    frames.clamp(LATENCY_MIN as i64 + 1000, LATENCY_MAX as i64) as u32
}

impl Sender {
    pub fn open(target: &Target, percent: u32, latency_ms: i32, auth: &Auth) -> Result<Sender> {
        let link = match target.kind {
            Kind::Ap2 => airplay2(target, percent, auth)?,
            Kind::Raop => raop(target, percent)?,
        };
        let Link {
            rtsp,
            audio,
            control,
            timing,
            control_target,
            ssrc,
            base_seq,
            base_ts,
            cipher,
            events,
            raop,
            receiver_frames,
        } = link;
        let sync_latency = sync_latency(latency_ms, receiver_frames);
        tracing::info!(
            "airplay: {} plays {} ms after its sync point, asking for {} ms",
            target.name,
            receiver_frames * 1000 / RATE,
            sync_latency * 1000 / RATE
        );
        control
            .set_read_timeout(Some(Duration::from_millis(100)))
            .ok();
        let control_reader = control.try_clone()?;
        let shared = Arc::new(Shared {
            rtsp: Mutex::new(rtsp),
            audio,
            control,
            control_target,
            cipher,
            ssrc,
            raop,
            latency_frames: AtomicU32::new(sync_latency),
            receiver_frames,
            state: Mutex::new(StreamState {
                down: Downsampler::new(),
                input: Vec::new(),
                pending: Vec::new(),
                seq: base_seq,
                ts: base_ts,
                anchor: None,
                first: true,
                since_sync: 0,
            }),
            history: Mutex::new(VecDeque::new()),
            stop: AtomicBool::new(false),
            alive: AtomicBool::new(true),
        });
        let mut threads = Vec::new();
        let worker = shared.clone();
        threads.push(
            std::thread::Builder::new()
                .name("airplay-control".into())
                .spawn(move || control_loop(worker, control_reader))?,
        );
        let worker = shared.clone();
        threads.push(
            std::thread::Builder::new()
                .name("airplay-keepalive".into())
                .spawn(move || keepalive_loop(worker))?,
        );
        if let Some((stream, read, write)) = events {
            let worker = shared.clone();
            threads.push(
                std::thread::Builder::new()
                    .name("airplay-events".into())
                    .spawn(move || events_loop(worker, stream, read, write))?,
            );
        }
        Ok(Sender {
            shared,
            threads: Mutex::new(threads),
            _timing: timing,
            device_volume: percent,
        })
    }

    pub fn alive(&self) -> bool {
        self.shared.alive.load(Ordering::Acquire) && !self.shared.stop.load(Ordering::Acquire)
    }

    pub fn volume(&self) -> u32 {
        self.device_volume
    }

    pub fn set_latency(&self, latency_ms: i32) {
        let frames = sync_latency(latency_ms, self.shared.receiver_frames);
        self.shared.latency_frames.store(frames, Ordering::Relaxed);
        lock(&self.shared.state).since_sync = SYNC_EVERY;
    }

    pub fn set_volume(&self, percent: u32) {
        let mut rtsp = lock(&self.shared.rtsp);
        if let Err(e) = send_volume(&mut rtsp, percent) {
            tracing::warn!("airplay: setting the volume failed: {e:#}");
            if fatal(&e) {
                self.shared.alive.store(false, Ordering::Release);
            }
        }
    }

    pub fn push(&self, due_us: i64, pcm: &[u8]) {
        if !self.alive() {
            return;
        }
        let shared = &*self.shared;
        let mut st = lock(&shared.state);
        let st = &mut *st;
        st.input.clear();
        st.input.extend(
            pcm.as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b)),
        );
        let buffered_us = (st.pending.len() / CHANNELS) as i64 * 1_000_000 / i64::from(RATE);
        let tail_due = due_us - buffered_us;
        match &st.anchor {
            None => {
                st.anchor = Some(Anchor {
                    due_us: tail_due,
                    ts: st.ts,
                });
                st.first = true;
            }
            Some(anchor) => {
                let frames = i64::from(st.ts.wrapping_sub(anchor.ts) as i32);
                let expected = anchor.due_us + frames * 1_000_000 / i64::from(RATE);
                if (expected - tail_due).abs() > REANCHOR_US {
                    st.anchor = Some(Anchor {
                        due_us: tail_due,
                        ts: st.ts,
                    });
                    st.first = true;
                }
            }
        }
        let StreamState {
            down,
            input,
            pending,
            ..
        } = st;
        down.process(input, pending);
        while st.pending.len() >= PACKET_FRAMES * CHANNELS {
            self.send_packet(st);
        }
    }

    fn send_packet(&self, st: &mut StreamState) {
        let shared = &*self.shared;
        let anchor = st.anchor.as_ref().expect("anchored before sending");
        let frames = i64::from(st.ts.wrapping_sub(anchor.ts) as i32);
        let due_us = anchor.due_us + frames * 1_000_000 / i64::from(RATE);
        if st.first || st.since_sync >= SYNC_EVERY {
            let wall = rtp::ntp_add_micros(rtp::ntp_now(), due_us - now_us());
            let sync = rtp::sync_packet(
                st.first,
                st.ts
                    .wrapping_sub(shared.latency_frames.load(Ordering::Relaxed)),
                wall,
                st.ts,
            );
            let _ = shared.control.send_to(&sync, shared.control_target);
            st.since_sync = 0;
        }
        st.since_sync += 1;
        let samples: Vec<i16> = st.pending.drain(..PACKET_FRAMES * CHANNELS).collect();
        let payload = alac::encode_uncompressed(&samples);
        let header = rtp::audio_header(st.first, st.seq, st.ts, shared.ssrc);
        let packet = shared.cipher.seal_packet(&header, st.seq, &payload);
        if let Err(e) = shared.audio.send(&packet) {
            tracing::debug!("airplay: sending audio failed: {e}");
            shared.alive.store(false, Ordering::Release);
        }
        let mut history = lock(&shared.history);
        history.push_back((st.seq, packet));
        while history.len() > HISTORY {
            history.pop_front();
        }
        drop(history);
        st.first = false;
        st.seq = st.seq.wrapping_add(1);
        st.ts = st.ts.wrapping_add(PACKET_FRAMES as u32);
    }

    pub fn flush(&self) {
        if !self.alive() {
            return;
        }
        let (seq, ts) = {
            let mut st = lock(&self.shared.state);
            st.pending.clear();
            st.down.reset();
            st.anchor = None;
            st.first = true;
            st.since_sync = 0;
            (st.seq, st.ts)
        };
        lock(&self.shared.history).clear();
        let info = format!("seq={seq};rtptime={ts}");
        let mut rtsp = lock(&self.shared.rtsp);
        if let Err(e) = rtsp.request("FLUSH", None, &[("RTP-Info", info)], None) {
            tracing::warn!("airplay: flush failed: {e:#}");
            if fatal(&e) {
                self.shared.alive.store(false, Ordering::Release);
            }
        }
    }

    pub fn close(&self) {
        if self.shared.stop.swap(true, Ordering::AcqRel) {
            return;
        }
        {
            let mut rtsp = lock(&self.shared.rtsp);
            rtsp.set_timeout(Duration::from_secs(1));
            let _ = rtsp.request("TEARDOWN", None, &[], None);
            rtsp.shutdown();
        }
        for thread in lock(&self.threads).drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.close();
    }
}

fn control_loop(shared: Arc<Shared>, socket: UdpSocket) {
    let mut buffer = [0u8; 64];
    while !shared.stop.load(Ordering::Acquire) {
        let Ok((len, from)) = socket.recv_from(&mut buffer) else {
            continue;
        };
        let Some((first, count)) = rtp::resend_request(&buffer[..len]) else {
            continue;
        };
        for offset in 0..count {
            let seq = first.wrapping_add(offset);
            let stored = lock(&shared.history)
                .iter()
                .find(|(found, _)| *found == seq)
                .map(|(_, packet)| packet.clone());
            match stored {
                Some(packet) if shared.raop => {
                    let _ = socket.send_to(&rtp::resend_packet(&packet), from);
                }
                Some(packet) => {
                    let _ = shared.audio.send(&packet);
                }
                None if !shared.raop => {
                    let _ = socket.send_to(&rtp::futile_resend(seq), from);
                }
                None => {}
            }
        }
    }
}

const MISSED_ANSWERS: u32 = 3;

fn keepalive_loop(shared: Arc<Shared>) {
    let (every, method, target) = if shared.raop {
        (KEEPALIVE_EVERY, "OPTIONS", "*")
    } else {
        (FEEDBACK_EVERY, "POST", "/feedback")
    };
    let started = Instant::now();
    let mut last = Instant::now();
    let mut missed = 0u32;
    while !shared.stop.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
        if last.elapsed() < every {
            continue;
        }
        last = Instant::now();
        let mut rtsp = lock(&shared.rtsp);
        match rtsp.request(method, Some(target), &[], None) {
            Ok(_) => missed = 0,
            Err(e)
                if !fatal(&e)
                    && missed + 1 < MISSED_ANSWERS
                    && !e.chain().any(|c| c.downcast_ref::<Refused>().is_some()) =>
            {
                missed += 1;
                tracing::warn!(
                    "airplay: no answer to {method} after {:?} ({missed}/{MISSED_ANSWERS}): {e:#}",
                    started.elapsed()
                );
            }
            Err(e) if e.downcast_ref::<Refused>().is_some() => {
                tracing::debug!("airplay: {method} was refused: {e:#}");
            }
            Err(e) => {
                tracing::warn!(
                    "airplay: the device stopped answering after {:?}: {e:#}",
                    started.elapsed()
                );
                shared.alive.store(false, Ordering::Release);
                return;
            }
        }
    }
}

fn events_loop(
    shared: Arc<Shared>,
    mut stream: TcpStream,
    mut read: FrameCipher,
    mut write: FrameCipher,
) {
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok();
    let mut raw: Vec<u8> = Vec::new();
    let mut inbox: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    while !shared.stop.load(Ordering::Acquire) {
        match stream.read(&mut chunk) {
            Ok(0) => {
                tracing::info!("airplay: the device closed the event channel");
                return;
            }
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => {
                tracing::info!("airplay: the event channel failed: {e}");
                return;
            }
        }
        while raw.len() >= 2 {
            let length = [raw[0], raw[1]];
            let size = usize::from(u16::from_le_bytes(length));
            if size > MAX_FRAME {
                tracing::warn!("airplay: an event frame of {size} bytes");
                return;
            }
            let whole = 2 + size + TAG_LEN;
            if raw.len() < whole {
                break;
            }
            let sealed: Vec<u8> = raw[2..whole].to_vec();
            raw.drain(..whole);
            let Some(plain) = read.open(length, &sealed) else {
                tracing::warn!("airplay: an event frame failed to decrypt");
                return;
            };
            inbox.extend_from_slice(&plain);
        }
        while let Ok(Some(message)) = take_message(&mut inbox) {
            tracing::debug!("airplay event: {}", message.first_line);
            let cseq = message.headers.get("cseq").cloned().unwrap_or_default();
            let reply = format!(
                "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nAudio-Latency: 0\r\nServer: AirTunes/366.0\r\nContent-Length: 0\r\n\r\n"
            );
            if stream.write_all(&write.seal(reply.as_bytes())).is_err() {
                tracing::info!("airplay: could not answer an event");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_line_gives_the_code() {
        assert_eq!(parse_status("RTSP/1.0 200 OK").unwrap(), 200);
        assert_eq!(
            parse_status("HTTP/1.1 470 Connection Authorization Required").unwrap(),
            470
        );
        assert!(parse_status("SSH-2.0 x").is_err());
    }

    #[test]
    fn an_answer_is_taken_only_once_its_body_is_complete() {
        let mut inbox = b"RTSP/1.0 200 OK\r\nCSeq: 3\r\nContent-Length: 4\r\n\r\nab".to_vec();
        assert!(take_message(&mut inbox).unwrap().is_none());
        inbox.extend_from_slice(b"cdRTSP/1.0 403 Forbidden\r\n\r\n");
        let first = take_message(&mut inbox).unwrap().unwrap();
        assert_eq!(first.body, b"abcd");
        let second = take_message(&mut inbox).unwrap().unwrap();
        assert_eq!(parse_status(&second.first_line).unwrap(), 403);
        assert!(inbox.is_empty());
    }

    #[test]
    fn the_setup_answer_lists_the_ports() {
        let ports = transport_ports(
            "RTP/AVP/UDP;unicast;mode=record;server_port=6010;control_port=6011;timing_port=6012",
        );
        assert_eq!(ports.get("server_port"), Some(&6010));
        assert_eq!(ports.get("control_port"), Some(&6011));
    }

    #[test]
    fn volume_is_linear_in_db_and_zero_mutes() {
        assert_eq!(volume_db(0), -144.0);
        assert_eq!(volume_db(100), 0.0);
        assert_eq!(volume_db(50), -15.0);
        assert_eq!(percent_from_db(-15.0), 50);
        assert_eq!(percent_from_db(-144.0), 0);
    }

    #[test]
    fn the_receivers_own_delay_is_taken_out_of_the_latency_we_ask_for() {
        assert_eq!(sync_latency(1000, 12_000), 44_100 - 12_000);
        assert_eq!(sync_latency(1000, 0), 44_100);
        assert_eq!(sync_latency(300, 30_000), LATENCY_MIN as u32);
    }

    #[test]
    fn latency_stays_inside_what_the_stream_setup_promises() {
        assert_eq!(latency_frames(1000), 44_100);
        assert_eq!(latency_frames(10_000), LATENCY_MAX as u32);
        assert_eq!(latency_frames(-500), LATENCY_MIN as u32 + 1000);
    }

    #[test]
    fn downsampling_keeps_the_pitch_and_the_length() {
        let mut down = Downsampler::new();
        let mut out = Vec::new();
        for block in 0..50 {
            let input: Vec<i16> = (0..960)
                .flat_map(|n| {
                    let t = (block * 960 + n) as f64 / 48_000.0;
                    let v = ((2.0 * std::f64::consts::PI * 1000.0 * t).sin() * 10_000.0) as i16;
                    [v, v]
                })
                .collect();
            down.process(&input, &mut out);
        }
        let frames = out.len() / 2;
        assert!(
            (frames as i64 - 44_100 * 50 * 960 / 48_000).abs() < 64,
            "{frames}"
        );
        let peak = out[out.len() / 2..]
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .unwrap();
        assert!((9_500..=10_500).contains(&peak), "{peak}");
        let crossings = out
            .as_chunks::<2>()
            .0
            .iter()
            .skip(2000)
            .map(|f| f[0])
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| w[0] < 0 && w[1] >= 0)
            .count();
        let seconds = (frames - 2000) as f64 / 44_100.0;
        assert!((crossings as f64 / seconds - 1000.0).abs() < 15.0);
    }

    #[test]
    fn silence_stays_silent() {
        let mut down = Downsampler::new();
        let mut out = Vec::new();
        down.process(&vec![0i16; 4096], &mut out);
        assert!(out.iter().all(|s| *s == 0));
    }
}
