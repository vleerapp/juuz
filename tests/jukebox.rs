mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use serde_json::Value;

#[derive(Default)]
struct Mock {
    log: Vec<String>,
    uris: Vec<String>,
    state: String,
    volume: u32,
}

struct Renderer {
    url: String,
    mock: Arc<Mutex<Mock>>,
}

fn tag(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&format!("</{name}>"))? + start;
    Some(
        body[start..end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn serve(mut stream: TcpStream, base: &str, mock: &Mutex<Mock>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let length: usize = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap_or(0))
        })
        .unwrap_or(0);
    while buf.len() < header_end + length {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();

    if head.starts_with("GET /desc.xml") {
        let xml = format!(
            r#"<?xml version="1.0"?><root xmlns="urn:schemas-upnp-org:device-1-0"><URLBase>{base}/</URLBase><device>
<deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType><friendlyName>mock speakers</friendlyName><UDN>uuid:mock-1</UDN>
<serviceList>
<service><serviceType>urn:schemas-upnp-org:service:AVTransport:1</serviceType><controlURL>/av</controlURL></service>
<service><serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType><controlURL>/rc</controlURL></service>
</serviceList></device></root>"#
        );
        respond(&mut stream, "200 OK", &xml);
        return;
    }

    let action = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .starts_with("soapaction:")
                .then(|| l.to_string())
        })
        .and_then(|l| {
            l.split('#')
                .nth(1)
                .map(|a| a.trim_matches(|c| c == '"' || c == ' ').to_string())
        })
        .unwrap_or_default();
    let mut m = mock.lock().unwrap();
    m.log.push(action.clone());
    let reply = match action.as_str() {
        "SetAVTransportURI" => {
            m.uris.push(tag(&body, "CurrentURI").unwrap_or_default());
            m.state = "STOPPED".into();
            String::new()
        }
        "Play" => {
            m.state = "PLAYING".into();
            String::new()
        }
        "Pause" => {
            m.state = "PAUSED_PLAYBACK".into();
            String::new()
        }
        "Stop" => {
            m.state = "STOPPED".into();
            String::new()
        }
        "GetTransportInfo" => format!("<CurrentTransportState>{}</CurrentTransportState>", m.state),
        "GetPositionInfo" => format!(
            "<TrackURI>{}</TrackURI><RelTime>0:00:05</RelTime>",
            m.uris.last().cloned().unwrap_or_default()
        ),
        "GetVolume" => format!("<CurrentVolume>{}</CurrentVolume>", m.volume),
        "SetVolume" => {
            m.volume = tag(&body, "DesiredVolume")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            String::new()
        }
        _ => String::new(),
    };
    drop(m);
    let envelope = format!(
        r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:{action}Response xmlns:u="x">{reply}</u:{action}Response></s:Body></s:Envelope>"#
    );
    respond(&mut stream, "200 OK", &envelope);
}

fn renderer() -> Renderer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mock = Arc::new(Mutex::new(Mock {
        state: "STOPPED".into(),
        volume: 30,
        ..Mock::default()
    }));
    let (b, m) = (base.clone(), mock.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (b, m) = (b.clone(), m.clone());
            std::thread::spawn(move || serve(stream, &b, &m));
        }
    });
    Renderer {
        url: format!("{base}/desc.xml"),
        mock,
    }
}

async fn library(h: &Harness) -> Vec<String> {
    let root = h.root();
    for (i, name) in ["One", "Two", "Three"].iter().enumerate() {
        make_track(
            &root,
            &format!("Band/2020 - Album/0{} - {name}.wav", i + 1),
            i as u32 + 1,
            &Tags {
                title: Some(name),
                artist: Some("Band"),
                album: Some("Album"),
                track: Some(i as u32 + 1),
                ..Tags::default()
            },
        );
    }
    h.scan().await;
    let v = h.ok("getRandomSongs", "size=10").await;
    let mut songs: Vec<(String, String)> = v["randomSongs"]["song"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["title"].as_str().unwrap().to_string(),
                s["id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    songs.sort();
    songs.into_iter().map(|s| s.1).collect()
}

fn ids_query(ids: &[&String]) -> String {
    ids.iter()
        .map(|i| format!("id={i}"))
        .collect::<Vec<_>>()
        .join("&")
}

async fn until<F: Fn() -> bool>(f: F) {
    for _ in 0..120 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("condition not reached");
}

#[tokio::test(flavor = "multi_thread")]
async fn works_without_an_output() {
    let h = Harness::new();
    let status = h.ok("jukeboxControl", "action=status").await;
    assert_eq!(status["jukeboxStatus"]["playing"], false);
    assert_eq!(status["jukeboxStatus"]["position"], 0);
    let get = h.ok("jukeboxControl", "action=get").await;
    assert_eq!(get["jukeboxPlaylist"]["currentIndex"], 0);
    assert!(get["jukeboxPlaylist"].get("entry").is_none());
    assert_eq!(h.ok("jukeboxControl", "action=clear").await["status"], "ok");
    assert_eq!(
        h.call("jukeboxControl", "action=bogus").await["status"],
        "failed"
    );
    assert_eq!(
        h.call("jukeboxControl", "action=skip&index=0").await["status"],
        "failed"
    );
    let outputs = h.ok("juuzGetJukeboxOutputs", "").await;
    assert!(outputs["juuzJukeboxOutputs"].get("current").is_none());
    let ext = h.ok("getOpenSubsonicExtensions", "").await;
    let names: Vec<&str> = ext["openSubsonicExtensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"juuzJukeboxOutput") && names.contains(&"juuzJukeboxEvents"));
}

#[tokio::test(flavor = "multi_thread")]
async fn starting_without_an_output_fails() {
    let h = Harness::new();
    let ids = library(&h).await;
    h.ok(
        "jukeboxControl",
        &format!("action=set&{}", ids_query(&[&ids[0]])),
    )
    .await;
    assert_eq!(
        h.call("jukeboxControl", "action=start").await["status"],
        "failed"
    );
    assert_eq!(
        h.ok("jukeboxControl", "action=status").await["jukeboxStatus"]["playing"],
        false
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn feishin_flow_drives_a_renderer() {
    let r = renderer();
    let h = Harness::new();
    let ids = library(&h).await;
    let (a, b, c) = (&ids[0], &ids[1], &ids[2]);

    let set = h
        .ok("juuzSetJukeboxOutput", &format!("url={}", r.url))
        .await;
    assert_eq!(
        set["juuzJukeboxOutputs"]["current"]["name"],
        "mock speakers"
    );
    assert_eq!(set["juuzJukeboxOutputs"]["current"]["online"], true);

    assert_eq!(
        h.ok("jukeboxControl", "action=status").await["jukeboxStatus"]["gain"],
        0.3
    );
    h.ok("jukeboxControl", "action=get").await;
    h.ok("jukeboxControl", "action=clear").await;

    h.ok(
        "jukeboxControl",
        &format!("action=set&{}", ids_query(&[a, b])),
    )
    .await;
    h.ok("jukeboxControl", "action=skip&index=0&offset=0").await;
    let started = h.ok("jukeboxControl", "action=start").await;
    assert_eq!(started["jukeboxStatus"]["playing"], true);

    let url = r.mock.lock().unwrap().uris.last().cloned().unwrap();
    assert!(url.contains(&format!("/jukebox/stream/{a}/")), "{url}");
    assert!(r.mock.lock().unwrap().log.contains(&"Play".to_string()));

    let path = url
        .split_once("/jukebox")
        .map(|(_, p)| format!("/jukebox{p}"))
        .unwrap();
    let served = h.get(&path).await;
    assert_eq!(served.status, 200);
    assert!(served.headers.contains_key("contentFeatures.dlna.org"));
    assert!(!served.body.is_empty());
    let forged = path.replace(&format!("/{a}/"), &format!("/{b}/"));
    assert_eq!(h.get(&forged).await.status, 403);

    let playlist = h.ok("jukeboxControl", "action=get").await;
    let entries = playlist["jukeboxPlaylist"]["entry"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["id"].as_str().unwrap(), a);
    assert_eq!(playlist["jukeboxPlaylist"]["playing"], true);

    r.mock.lock().unwrap().state = "STOPPED".into();
    until(|| r.mock.lock().unwrap().uris.len() >= 2).await;
    let advanced = h.ok("jukeboxControl", "action=get").await;
    assert_eq!(advanced["jukeboxPlaylist"]["currentIndex"], 1);
    assert_eq!(advanced["jukeboxPlaylist"]["playing"], true);
    assert!(r.mock.lock().unwrap().uris[1].contains(&format!("/jukebox/stream/{b}/")));

    let uris = r.mock.lock().unwrap().uris.len();
    h.ok("jukeboxControl", "action=skip&index=1&offset=42")
        .await;
    assert_eq!(r.mock.lock().unwrap().uris.len(), uris);
    assert!(r.mock.lock().unwrap().log.contains(&"Seek".to_string()));
    let xml = h
        .get(&format!(
            "/rest/jukeboxControl?{}&action=get&f=xml",
            h.auth().replace("f=json", "f=xml")
        ))
        .await;
    assert!(xml.text().contains("<jukeboxPlaylist"), "{}", xml.text());

    h.ok("jukeboxControl", "action=remove&index=0").await;
    h.ok("jukeboxControl", &format!("action=add&id={c}")).await;
    let after = h.ok("jukeboxControl", "action=get").await;
    assert_eq!(after["jukeboxPlaylist"]["currentIndex"], 0);
    let ids_now: Vec<&str> = after["jukeboxPlaylist"]["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids_now, [b.as_str(), c.as_str()]);

    h.ok("jukeboxControl", "action=setGain&gain=0.5").await;
    assert_eq!(r.mock.lock().unwrap().volume, 50);

    let stopped = h.ok("jukeboxControl", "action=stop").await;
    assert_eq!(stopped["jukeboxStatus"]["playing"], false);
    assert_eq!(r.mock.lock().unwrap().state, "PAUSED_PLAYBACK");

    let cleared = h.ok("juuzSetJukeboxOutput", "id=").await;
    assert!(cleared["juuzJukeboxOutputs"].get("current").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn output_survives_restart_setting() {
    let r = renderer();
    let h = Harness::new();
    h.ok("juuzSetJukeboxOutput", &format!("url={}", r.url))
        .await;
    let saved = h
        .server
        .state
        .db
        .run(|c| Ok(juuz::db::settings::get(c, "jukebox_output")?))
        .await
        .unwrap();
    assert!(saved.unwrap().contains("uuid:mock-1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn only_admins_change_the_output() {
    let h = Harness::new();
    h.ok("createUser", "username=bob&password=pw&email=b@x")
        .await;
    let call = |ep: &str| {
        let uri = format!("/rest/{ep}?u=bob&p=pw&f=json&c=t&url=http://127.0.0.1:1/x.xml");
        let h = &h;
        async move { h.get(&uri).await.json() }
    };
    assert_eq!(call("juuzSetJukeboxOutput").await["error"]["code"], 50);
    assert_eq!(call("juuzScanJukeboxOutputs").await["error"]["code"], 50);
    assert_eq!(call("juuzGetJukeboxOutputs").await["status"], "ok");
    let _: Value = call("jukeboxControl").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn events_stream_starts_with_a_status() {
    use futures_util::StreamExt;

    let h = Harness::new();
    let req = axum::http::Request::builder()
        .uri(format!("/rest/juuzJukeboxEvents?{}", h.auth()))
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = tower::ServiceExt::oneshot(h.server.router(), req)
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let mut body = resp.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(3), body.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&first).to_string();
    assert!(text.contains("event: status"), "{text}");
    assert!(text.contains("\"playing\":false"), "{text}");
    h.server.state.jukebox.shutdown();
    let end = tokio::time::timeout(Duration::from_secs(3), async {
        while body.next().await.is_some() {}
    })
    .await;
    assert!(end.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_resumes_the_session() {
    let r = renderer();
    let h = Harness::new();
    let ids = library(&h).await;
    h.ok("juuzSetJukeboxOutput", &format!("url={}", r.url))
        .await;
    h.ok(
        "jukeboxControl",
        &format!("action=set&{}", ids_query(&[&ids[0], &ids[1]])),
    )
    .await;
    h.ok("jukeboxControl", "action=skip&index=1&offset=0").await;
    h.ok("jukeboxControl", "action=start").await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    let st = &h.server.state;
    let fresh =
        juuz::jukebox::Jukebox::new(st.db.clone(), st.crypto.clone(), 4533, st.live.clone());
    fresh.start();
    for _ in 0..100 {
        let snap = fresh.snapshot().await;
        if snap.playing && snap.queue_length == 2 {
            assert_eq!(snap.current_index, 1);
            assert_eq!(snap.output.as_deref(), Some("mock speakers"));
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("session was not resumed: {:?}", fresh.snapshot().await);
}

async fn now_playing(h: &Harness) -> Vec<Value> {
    h.ok("getNowPlaying", "").await["nowPlaying"]["entry"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn jukebox_plays_show_in_stats_and_now_playing() {
    let r = renderer();
    let h = Harness::new();
    let ids = library(&h).await;
    let (a, b) = (&ids[0], &ids[1]);
    h.ok("juuzSetJukeboxOutput", &format!("url={}", r.url))
        .await;
    h.ok(
        "jukeboxControl",
        &format!("action=set&{}", ids_query(&[a, b])),
    )
    .await;
    h.ok("jukeboxControl", "action=skip&index=0&offset=0").await;
    h.ok("jukeboxControl", "action=start").await;

    let mut entries = Vec::new();
    for _ in 0..50 {
        entries = now_playing(&h).await;
        if !entries.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"].as_str().unwrap(), a);
    assert_eq!(entries[0]["playerName"], "jukebox");
    assert_eq!(entries[0]["username"], USER);

    r.mock.lock().unwrap().state = "STOPPED".into();
    until(|| r.mock.lock().unwrap().uris.len() >= 2).await;
    for _ in 0..50 {
        let song = h.ok("getSong", &format!("id={a}")).await;
        if song["song"]["playCount"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        h.ok("getSong", &format!("id={a}")).await["song"]["playCount"],
        1
    );
    assert_eq!(
        h.ok("getSong", &format!("id={b}")).await["song"]["playCount"],
        0
    );
    let entries = now_playing(&h).await;
    assert_eq!(entries[0]["id"].as_str().unwrap(), b);

    h.ok("jukeboxControl", "action=stop").await;
    for _ in 0..50 {
        if now_playing(&h).await.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("now playing was not cleared");
}

#[tokio::test(flavor = "multi_thread")]
async fn continues_from_the_saved_play_queue() {
    let r = renderer();
    let h = Harness::new();
    let ids = library(&h).await;
    let (a, b, c) = (&ids[0], &ids[1], &ids[2]);
    h.ok("juuzSetJukeboxOutput", &format!("url={}", r.url))
        .await;
    h.ok(
        "savePlayQueueByIndex",
        &format!("{}&currentIndex=0", ids_query(&[a, b, c])),
    )
    .await;
    h.ok("jukeboxControl", &format!("action=set&id={a}")).await;
    h.ok("jukeboxControl", "action=skip&index=0&offset=0").await;
    h.ok("jukeboxControl", "action=start").await;

    r.mock.lock().unwrap().state = "STOPPED".into();
    until(|| r.mock.lock().unwrap().uris.len() >= 2).await;
    assert!(r.mock.lock().unwrap().uris[1].contains(&format!("/jukebox/stream/{b}/")));

    let playlist = h.ok("jukeboxControl", "action=get").await["jukeboxPlaylist"].clone();
    assert_eq!(playlist["currentIndex"], 1);
    assert_eq!(playlist["entry"].as_array().unwrap().len(), 3);
}

fn count(r: &Renderer, action: &str) -> usize {
    r.mock
        .lock()
        .unwrap()
        .log
        .iter()
        .filter(|a| *a == action)
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn redundant_seek_and_start_do_not_restart_the_song() {
    let r = renderer();
    let h = Harness::new();
    let ids = library(&h).await;
    let (a, b, c) = (&ids[0], &ids[1], &ids[2]);
    h.ok("juuzSetJukeboxOutput", &format!("url={}", r.url))
        .await;
    h.ok(
        "jukeboxControl",
        &format!("action=set&{}", ids_query(&[a, b])),
    )
    .await;
    h.ok("jukeboxControl", "action=skip&index=0&offset=0").await;
    h.ok("jukeboxControl", "action=start").await;
    assert_eq!(count(&r, "Play"), 1);

    r.mock.lock().unwrap().state = "STOPPED".into();
    until(|| r.mock.lock().unwrap().uris.len() >= 2).await;
    assert_eq!(count(&r, "Play"), 2);

    h.ok("jukeboxControl", "action=remove&index=0").await;
    h.ok("jukeboxControl", &format!("action=add&id={c}")).await;
    h.ok("jukeboxControl", "action=skip&index=0&offset=0").await;
    h.ok("jukeboxControl", "action=start").await;
    assert_eq!(count(&r, "Seek"), 0);
    assert_eq!(count(&r, "Play"), 2);
    assert_eq!(r.mock.lock().unwrap().uris.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn seeking_to_the_end_advances_without_a_long_pause() {
    let r = renderer();
    let h = Harness::new();
    let ids = library(&h).await;
    h.ok("juuzSetJukeboxOutput", &format!("url={}", r.url))
        .await;
    h.ok(
        "jukeboxControl",
        &format!("action=set&{}", ids_query(&[&ids[0], &ids[1]])),
    )
    .await;
    h.ok("jukeboxControl", "action=skip&index=0&offset=0").await;
    h.ok("jukeboxControl", "action=start").await;
    h.ok("jukeboxControl", "action=skip&index=0&offset=40")
        .await;
    assert_eq!(count(&r, "Seek"), 1);
    r.mock.lock().unwrap().state = "STOPPED".into();
    let began = std::time::Instant::now();
    until(|| r.mock.lock().unwrap().uris.len() >= 2).await;
    assert!(
        began.elapsed() < Duration::from_millis(2500),
        "{:?}",
        began.elapsed()
    );
}

async fn client(h: &Harness, player: &str, query: &str) -> Value {
    h.get(&format!(
        "/rest/scrobble?u={USER}&p={PASS}&f=json&c={player}&{query}"
    ))
    .await
    .json()
}

#[tokio::test(flavor = "multi_thread")]
async fn now_playing_lists_every_device() {
    let h = Harness::new();
    let ids = library(&h).await;
    assert_eq!(
        client(&h, "phone", &format!("id={}&submission=false", ids[0])).await["status"],
        "ok"
    );
    assert_eq!(
        client(&h, "laptop", &format!("id={}&submission=false", ids[1])).await["status"],
        "ok"
    );
    let players: std::collections::BTreeSet<String> = now_playing(&h)
        .await
        .iter()
        .map(|e| e["playerName"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(players, ["laptop".to_string(), "phone".to_string()].into());

    client(&h, "phone", &format!("id={}", ids[0])).await;
    let left = now_playing(&h).await;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0]["playerName"], "laptop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_jukebox_play_and_the_clients_scrobble_count_once() {
    let r = renderer();
    let h = Harness::new();
    let ids = library(&h).await;
    let (a, b) = (&ids[0], &ids[1]);
    h.ok("juuzSetJukeboxOutput", &format!("url={}", r.url))
        .await;
    h.ok(
        "jukeboxControl",
        &format!("action=set&{}", ids_query(&[a, b])),
    )
    .await;
    h.ok("jukeboxControl", "action=skip&index=0&offset=0").await;
    h.ok("jukeboxControl", "action=start").await;

    client(&h, "feishin", &format!("id={a}&submission=false")).await;
    let entries = now_playing(&h).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["playerName"], "jukebox");

    r.mock.lock().unwrap().state = "STOPPED".into();
    until(|| r.mock.lock().unwrap().uris.len() >= 2).await;
    for _ in 0..50 {
        if h.ok("getSong", &format!("id={a}")).await["song"]["playCount"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    client(&h, "feishin", &format!("id={a}")).await;
    assert_eq!(
        h.ok("getSong", &format!("id={a}")).await["song"]["playCount"],
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn clients_that_only_stream_show_as_playing() {
    let h = Harness::new();
    let ids = library(&h).await;
    let play = |player: &'static str, id: String| {
        let h = &h;
        async move {
            h.get(&format!(
                "/rest/stream?u={USER}&p={PASS}&c={player}&id={id}"
            ))
            .await
            .status
        }
    };
    assert_eq!(play("knips", ids[0].clone()).await, 200);
    assert_eq!(play("knips", ids[0].clone()).await, 200);
    assert_eq!(play("other", ids[1].clone()).await, 200);

    let entries = now_playing(&h).await;
    let mut seen: Vec<(String, String)> = entries
        .iter()
        .map(|e| {
            (
                e["playerName"].as_str().unwrap().to_string(),
                e["id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        [
            ("knips".to_string(), ids[0].clone()),
            ("other".to_string(), ids[1].clone())
        ]
    );

    client(&h, "knips", &format!("id={}", ids[0])).await;
    let left = now_playing(&h).await;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0]["playerName"], "other");
}
