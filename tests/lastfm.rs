mod common;

use std::collections::BTreeMap;

use common::*;
use md5::{Digest, Md5};

const KEY: &str = "testkey";
const SECRET: &str = "testsecret";

fn mock() -> MockOmm {
    mock_server(|_| {
        vec![
            route("/2.0/?method=auth.getToken", 200, r#"{"token":"tok123"}"#),
            route(
                "/2.0/?method=auth.getSession",
                200,
                r#"{"session":{"name":"lfmuser","key":"SK123","subscriber":0}}"#,
            ),
            route("/2.0/", 200, r#"{"scrobbles":{"@attr":{"accepted":1}}}"#),
        ]
    })
}

fn harness(mock: &MockOmm) -> Harness {
    let url = mock.url.clone();
    Harness::build(None, move |c| {
        c.lastfm.api_key = Some(KEY.into());
        c.lastfm.secret = Some(SECRET.into());
        c.lastfm.api_url = format!("{url}/2.0/");
        c.lastfm.auth_url = "https://www.last.fm/api/auth/".into();
    })
}

fn form(request: &str) -> BTreeMap<String, String> {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn signature_ok(params: &BTreeMap<String, String>) -> bool {
    let mut text = String::new();
    for (k, v) in params {
        if k != "api_sig" && k != "format" {
            text.push_str(k);
            text.push_str(v);
        }
    }
    text.push_str(SECRET);
    hex::encode(Md5::digest(text.as_bytes())) == params["api_sig"]
}

async fn posts(mock: &MockOmm, method: &str, want: usize) -> Vec<BTreeMap<String, String>> {
    for _ in 0..100 {
        let found: Vec<_> = mock
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("POST"))
            .map(|r| form(r))
            .filter(|f| f.get("method").map(String::as_str) == Some(method))
            .collect();
        if found.len() >= want {
            return found;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Vec::new()
}

fn long_track(h: &Harness) {
    make_track(
        h.music.path(),
        "Artist/2020 - Album/01 - Song.wav",
        1,
        &Tags {
            title: Some("Song"),
            artist: Some("Artist"),
            album: Some("Album"),
            track: Some(1),
            ..Default::default()
        },
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn link_now_playing_scrobble_and_unlink() {
    let mock = mock();
    let h = harness(&mock);
    let path = h.music.path().join("Artist/2020 - Album");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("01 - Song.wav"),
        wav_bytes_spec(1, 40_000, 8000, 1),
    )
    .unwrap();
    tag_file(
        &path.join("01 - Song.wav"),
        &Tags {
            title: Some("Song"),
            artist: Some("Artist"),
            album: Some("Album"),
            track: Some(1),
            ..Default::default()
        },
    );
    h.scan().await;
    let sid = h.ok("search3", "query=song").await["searchResult3"]["song"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let ext = h.get("/rest/getOpenSubsonicExtensions?f=json").await.json();
    assert!(
        ext["openSubsonicExtensions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == "juuzLastfm")
    );

    let status = h.ok("juuzLastfmStatus", "").await;
    assert_eq!(status["juuzLastfm"]["configured"], true);
    assert_eq!(status["juuzLastfm"]["linked"], false);

    let auth = h.ok("juuzLastfmAuthUrl", "").await;
    assert_eq!(auth["juuzLastfm"]["token"], "tok123");
    assert_eq!(
        auth["juuzLastfm"]["url"],
        "https://www.last.fm/api/auth/?api_key=testkey&token=tok123"
    );
    let token_request = mock.requests.lock().unwrap()[0].clone();
    assert!(token_request.starts_with("GET /2.0/?method=auth.getToken&api_key=testkey"));
    assert!(token_request.contains("api_sig="));

    let linked = h.ok("juuzLastfmLink", "token=tok123").await;
    assert_eq!(linked["juuzLastfm"]["username"], "lfmuser");
    let status = h.ok("juuzLastfmStatus", "").await;
    assert_eq!(status["juuzLastfm"]["linked"], true);
    assert_eq!(status["juuzLastfm"]["username"], "lfmuser");

    h.ok("scrobble", &format!("id={sid}&submission=false"))
        .await;
    let now = posts(&mock, "track.updateNowPlaying", 1).await;
    assert_eq!(now.len(), 1, "now playing must reach last.fm");
    assert_eq!(now[0]["sk"], "SK123");
    assert_eq!(now[0]["artist"], "Artist");
    assert_eq!(now[0]["track"], "Song");
    assert_eq!(now[0]["album"], "Album");
    assert_eq!(now[0]["duration"], "40");
    assert!(signature_ok(&now[0]));

    h.ok("scrobble", &format!("id={sid}&time=1700000000000"))
        .await;
    let scrobbles = posts(&mock, "track.scrobble", 1).await;
    assert_eq!(scrobbles.len(), 1);
    assert_eq!(scrobbles[0]["timestamp"], "1700000000");
    assert_eq!(scrobbles[0]["track"], "Song");
    assert!(signature_ok(&scrobbles[0]));
    assert_eq!(
        h.ok("getSong", &format!("id={sid}")).await["song"]["playCount"],
        1
    );

    h.ok("juuzLastfmUnlink", "").await;
    assert_eq!(
        h.ok("juuzLastfmStatus", "").await["juuzLastfm"]["linked"],
        false
    );
    let before = mock.requests.lock().unwrap().len();
    h.ok("scrobble", &format!("id={sid}")).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        mock.requests.lock().unwrap().len(),
        before,
        "unlinked users are not forwarded"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn short_tracks_are_not_scrobbled() {
    let mock = mock();
    let h = harness(&mock);
    long_track(&h);
    h.scan().await;
    let sid = h.ok("search3", "query=song").await["searchResult3"]["song"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    h.ok("juuzLastfmLink", "token=tok123").await;
    h.ok("scrobble", &format!("id={sid}")).await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let posted = mock
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.starts_with("POST"))
        .count();
    assert_eq!(posted, 0, "tracks under 30 seconds are never scrobbled");
    assert_eq!(
        h.ok("getSong", &format!("id={sid}")).await["song"]["playCount"],
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_session_unlinks_the_user() {
    let mock = mock_server(|_| {
        vec![
            route(
                "/2.0/?method=auth.getSession",
                200,
                r#"{"session":{"name":"lfmuser","key":"SK123"}}"#,
            ),
            route(
                "/2.0/",
                403,
                r#"{"error":9,"message":"Invalid session key"}"#,
            ),
        ]
    });
    let h = harness(&mock);
    let path = h.music.path().join("A/2020 - B");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("01 - S.wav"), wav_bytes_spec(2, 40_000, 8000, 1)).unwrap();
    tag_file(
        &path.join("01 - S.wav"),
        &Tags {
            title: Some("S"),
            artist: Some("A"),
            album: Some("B"),
            track: Some(1),
            ..Default::default()
        },
    );
    h.scan().await;
    let sid = h.ok("search3", "query=s").await["searchResult3"]["song"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    h.ok("juuzLastfmLink", "token=x").await;
    h.ok("scrobble", &format!("id={sid}")).await;
    for _ in 0..100 {
        if h.ok("juuzLastfmStatus", "").await["juuzLastfm"]["linked"] == false {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("a rejected session key must unlink the user");
}

#[tokio::test(flavor = "multi_thread")]
async fn not_configured_is_reported_cleanly() {
    let h = Harness::new();
    let status = h.ok("juuzLastfmStatus", "").await;
    assert_eq!(status["juuzLastfm"]["configured"], false);
    assert_eq!(h.call("juuzLastfmAuthUrl", "").await["status"], "failed");
    assert_eq!(
        h.call("juuzLastfmLink", "token=x").await["status"],
        "failed"
    );
    let ext = h.get("/rest/getOpenSubsonicExtensions?f=json").await.json();
    assert!(
        !ext["openSubsonicExtensions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == "juuzLastfm")
    );
    h.ok("juuzLastfmUnlink", "").await;
}
