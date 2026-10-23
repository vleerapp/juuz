#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use juuz::config::Config;
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::{Accessor, ItemKey, Tag, TagType};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

pub const USER: &str = "admin";
pub const PASS: &str = "pw";

pub struct Harness {
    pub server: juuz::Server,
    pub music: TempDir,
    pub data: TempDir,
    app: Router,
}

pub struct Raw {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: Vec<u8>,
}

impl Raw {
    pub fn json(&self) -> Value {
        serde_json::from_slice::<Value>(&self.body).expect("json body")["subsonic-response"].clone()
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

impl Harness {
    pub fn new() -> Harness {
        Harness::with_omm(None)
    }

    pub fn with_omm(base_url: Option<String>) -> Harness {
        Harness::build(base_url, |_| {})
    }

    pub fn build(base_url: Option<String>, tweak: impl FnOnce(&mut Config)) -> Harness {
        let music = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let mut cfg = Config {
            data_dir: data.path().to_path_buf(),
            music_dir: music.path().to_path_buf(),
            ..Config::default()
        };
        cfg.scan.on_start = false;
        cfg.scan.watch = false;
        cfg.admin.user = Some(USER.into());
        cfg.admin.password = Some(PASS.into());
        cfg.omm.enabled = false;
        if let Some(url) = base_url {
            cfg.omm.enabled = true;
            cfg.omm.base_url = url;
            cfg.omm.timeout_secs = 3;
            cfg.omm.upload_timeout_secs = 3;
        }
        tweak(&mut cfg);
        let server = juuz::init(cfg).unwrap();
        let app = server.router();
        Harness {
            server,
            music,
            data,
            app,
        }
    }

    pub fn root(&self) -> PathBuf {
        std::fs::canonicalize(self.music.path()).unwrap()
    }

    pub async fn raw(&self, req: Request<Body>) -> Raw {
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        Raw {
            status,
            headers,
            body,
        }
    }

    pub async fn get(&self, uri: &str) -> Raw {
        self.raw(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
    }

    pub async fn get_from(&self, ip: &str, uri: &str) -> Raw {
        self.get_via(ip, &[], uri).await
    }

    pub async fn get_via(&self, ip: &str, headers: &[(&str, &str)], uri: &str) -> Raw {
        let mut req = Request::builder().uri(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let mut req = req.body(Body::empty()).unwrap();
        let addr: std::net::SocketAddr = format!("{ip}:40000").parse().unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(addr));
        self.raw(req).await
    }

    pub fn auth(&self) -> String {
        format!("u={USER}&p={PASS}&f=json&c=test")
    }

    pub async fn call(&self, endpoint: &str, query: &str) -> Value {
        self.get(&format!("/rest/{endpoint}?{}&{query}", self.auth()))
            .await
            .json()
    }

    pub async fn ok(&self, endpoint: &str, query: &str) -> Value {
        let v = self.call(endpoint, query).await;
        assert_eq!(v["status"], "ok", "{endpoint}?{query} -> {v}");
        v
    }

    pub async fn scan(&self) -> u64 {
        let scanner = self.server.state.scanner.clone();
        tokio::task::spawn_blocking(move || scanner.scan_blocking(false).unwrap().unwrap().scanned)
            .await
            .unwrap()
    }

    pub async fn api_key(&self) -> String {
        let v = self.ok("juuzCreateApiKey", "name=test").await;
        v["juuzApiKey"]["key"].as_str().unwrap().to_string()
    }

    pub async fn upload(&self, query: &str, filename: &str, bytes: &[u8]) -> Value {
        self.upload_raw(query, &[], filename, bytes).await.json()
    }

    pub async fn upload_raw(
        &self,
        query: &str,
        fields: &[(&str, &str)],
        filename: &str,
        bytes: &[u8],
    ) -> Raw {
        let (content_type, body) = multipart(fields, filename, bytes);
        let req = Request::builder()
            .method("POST")
            .uri(format!("/rest/juuzUploadSong?f=json&{query}"))
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .unwrap();
        self.raw(req).await
    }
}

pub fn multipart(fields: &[(&str, &str)], filename: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    let boundary = "----juuztestboundary";
    let mut body = Vec::new();
    for (k, v) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n")
                .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

pub fn wav_bytes(seed: u32, millis: u32) -> Vec<u8> {
    wav_bytes_spec(seed, millis, 8000, 1)
}

pub fn wav_bytes_spec(seed: u32, millis: u32, rate: u32, channels: u16) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut w = hound::WavWriter::new(&mut cursor, spec).unwrap();
        let freq = 200.0 + f64::from(seed) * 37.0;
        for i in 0..(rate * millis / 1000) {
            let t = f64::from(i) / f64::from(rate);
            let sample = ((t * freq * std::f64::consts::TAU).sin() * 12000.0) as i16;
            for _ in 0..channels {
                w.write_sample(sample).unwrap();
            }
        }
        w.finalize().unwrap();
    }
    cursor.into_inner()
}

#[derive(Default, Clone)]
pub struct Tags<'a> {
    pub title: Option<&'a str>,
    pub artist: Option<&'a str>,
    pub album: Option<&'a str>,
    pub album_artist: Option<&'a str>,
    pub track: Option<u32>,
    pub year: Option<&'a str>,
    pub genre: Option<&'a str>,
}

pub fn tag_file(path: &Path, t: &Tags) {
    let mut file = lofty::read_from_path(path).unwrap();
    let mut tag = Tag::new(TagType::Id3v2);
    if let Some(v) = t.title {
        tag.set_title(v.into());
    }
    if let Some(v) = t.artist {
        tag.set_artist(v.into());
    }
    if let Some(v) = t.album {
        tag.set_album(v.into());
    }
    if let Some(v) = t.album_artist {
        tag.insert_text(ItemKey::AlbumArtist, v.into());
    }
    if let Some(v) = t.track {
        tag.set_track(v);
    }
    if let Some(v) = t.year {
        tag.insert_text(ItemKey::RecordingDate, v.into());
    }
    if let Some(v) = t.genre {
        tag.set_genre(v.into());
    }
    file.insert_tag(tag);
    file.save_to_path(path, WriteOptions::default()).unwrap();
}

pub fn make_track(root: &Path, rel: &str, seed: u32, tags: &Tags) -> PathBuf {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, wav_bytes(seed, 600)).unwrap();
    tag_file(&path, tags);
    path
}

pub fn tagged_wav_bytes(seed: u32, tags: &Tags) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.wav");
    std::fs::write(&path, wav_bytes(seed, 600)).unwrap();
    tag_file(&path, tags);
    std::fs::read(path).unwrap()
}

pub fn count_files(root: &Path) -> usize {
    walk(root).len()
}

pub fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

pub struct MockOmm {
    pub url: String,
    pub hits: Arc<std::sync::Mutex<Vec<String>>>,
    pub requests: Arc<std::sync::Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for MockOmm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::net::TcpStream::connect(self.url.trim_start_matches("http://"));
    }
}

pub struct Route {
    pub prefix: &'static str,
    pub status: u16,
    pub body: Vec<u8>,
    pub once: bool,
}

pub fn route(prefix: &'static str, status: u16, body: impl Into<Vec<u8>>) -> Route {
    Route {
        prefix,
        status,
        body: body.into(),
        once: false,
    }
}

pub fn route_once(prefix: &'static str, status: u16, body: impl Into<Vec<u8>>) -> Route {
    Route {
        prefix,
        status,
        body: body.into(),
        once: true,
    }
}

pub fn mock_omm(routes: Vec<Route>) -> MockOmm {
    mock_server(|_| routes)
}

pub fn mock_server(make_routes: impl FnOnce(&str) -> Vec<Route>) -> MockOmm {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let routes = make_routes(&format!("http://{addr}"));
    let hits = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (h, s, r) = (hits.clone(), stop.clone(), requests.clone());
    let routes = std::sync::Mutex::new(routes);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if s.load(Ordering::Acquire) {
                break;
            }
            let Ok(mut stream) = stream else { continue };
            let head = read_request(&mut stream);
            r.lock().unwrap().push(head.clone());
            let target = head
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("")
                .to_string();
            h.lock().unwrap().push(target.clone());
            let (status, body) = {
                let mut routes = routes.lock().unwrap();
                match routes.iter().position(|r| target.starts_with(r.prefix)) {
                    Some(i) => {
                        let (status, body) = (routes[i].status, routes[i].body.clone());
                        if routes[i].once {
                            routes.remove(i);
                        }
                        (status, body)
                    }
                    None => (
                        404,
                        br#"{"error":{"status":404,"message":"nope"}}"#.to_vec(),
                    ),
                }
            };
            let extra = if status == 429 {
                "Retry-After: 1\r\n"
            } else {
                ""
            };
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/octet-stream\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    MockOmm {
        url: format!("http://{addr}"),
        hits,
        requests,
        stop,
    }
}

pub const OMM_SONG: &str = r#"{"data":{"id":"omm:song:0a1b2c3d4e5f6g7h","type":"song","attributes":{"name":"Digital Love","albumName":"Discovery","artistNames":["Daft Punk"],"isrc":"FRZ390100010","trackNumber":3,"discNumber":1,"genres":["Electronic"],"releaseDate":"2001-03-12","durationMs":500},"relationships":{"albums":{"data":[{"id":"omm:album:1a2b3c4d5e6f7g8h","type":"album","attributes":{"name":"Discovery","trackCount":14,"artistNames":["Daft Punk"],"upc":"724384960650","label":"Virgin"}}]},"artists":{"data":[{"id":"omm:artist:2a3b4c5d6e7f8g9h","type":"artist","attributes":{"name":"Daft Punk"}}]}}}}"#;

fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut data: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&data).into_owned();
        if let Some(split) = text.find("\r\n\r\n") {
            let length = text[..split]
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if data.len() >= split + 4 + length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&data).into_owned()
}
