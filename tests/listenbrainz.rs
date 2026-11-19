mod common;

use common::*;
use serde_json::Value;

fn mock() -> MockOmm {
    mock_server(|_| {
        vec![
            route(
                "/apis/listenbrainz/1/validate-token",
                200,
                r#"{"code":200,"message":"Token valid.","valid":true,"user_name":"lbuser"}"#,
            ),
            route(
                "/apis/listenbrainz/1/submit-listens",
                200,
                r#"{"status":"ok"}"#,
            ),
        ]
    })
}

fn body(request: &str) -> Value {
    serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap_or("")).unwrap_or(Value::Null)
}

async fn submissions(mock: &MockOmm, kind: &str) -> Vec<(String, Value)> {
    for _ in 0..100 {
        let found: Vec<_> = mock
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("POST /apis/listenbrainz/1/submit-listens"))
            .map(|r| (r.to_lowercase(), body(r)))
            .filter(|(_, b)| b["listen_type"] == kind)
            .collect();
        if !found.is_empty() {
            return found;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Vec::new()
}

fn song(h: &Harness, seconds: u32) {
    let path = h.music.path().join("Artist/2020 - Album");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("01 - Song.wav"),
        wav_bytes_spec(1, seconds * 1000, 8000, 1),
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
}

async fn first_song(h: &Harness) -> String {
    h.scan().await;
    h.ok("search3", "query=song").await["searchResult3"]["song"][0]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn link_args(mock: &MockOmm) -> String {
    format!(
        "url={}/apis/listenbrainz/&token=secret-token",
        mock.url.replace(':', "%3A").replace('/', "%2F")
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn link_now_playing_scrobble_and_unlink() {
    let mock = mock();
    let h = Harness::new();
    song(&h, 40);
    let sid = first_song(&h).await;

    let ext = h.get("/rest/getOpenSubsonicExtensions?f=json").await.json();
    assert!(
        ext["openSubsonicExtensions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == "juuzListenbrainz")
    );
    assert_eq!(
        h.ok("juuzListenbrainzStatus", "").await["juuzListenbrainz"]["linked"],
        false
    );

    let linked = h
        .ok(
            "juuzListenbrainzLink",
            &format!(
                "url={}/apis/listenbrainz&token=secret-token",
                form_urlencoded::byte_serialize(mock.url.as_bytes()).collect::<String>()
            ),
        )
        .await;
    assert_eq!(linked["juuzListenbrainz"]["linked"], true);
    assert_eq!(
        linked["juuzListenbrainz"]["url"],
        format!("{}/apis/listenbrainz", mock.url)
    );
    let check = mock.requests.lock().unwrap()[0].to_lowercase();
    assert!(check.starts_with("get /apis/listenbrainz/1/validate-token"));
    assert!(check.contains("authorization: token secret-token"));

    h.ok("scrobble", &format!("id={sid}&submission=false"))
        .await;
    let now = submissions(&mock, "playing_now").await;
    assert_eq!(now.len(), 1);
    assert!(now[0].0.contains("authorization: token secret-token"));
    let meta = &now[0].1["payload"][0]["track_metadata"];
    assert_eq!(meta["artist_name"], "Artist");
    assert_eq!(meta["track_name"], "Song");
    assert_eq!(meta["release_name"], "Album");
    assert_eq!(meta["additional_info"]["duration"], 40);
    assert!(now[0].1["payload"][0].get("listened_at").is_none());

    h.ok("scrobble", &format!("id={sid}&time=1700000000000"))
        .await;
    let single = submissions(&mock, "single").await;
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].1["payload"][0]["listened_at"], 1_700_000_000);
    assert_eq!(
        single[0].1["payload"][0]["track_metadata"]["track_name"],
        "Song"
    );

    h.ok("juuzListenbrainzUnlink", "").await;
    assert_eq!(
        h.ok("juuzListenbrainzStatus", "").await["juuzListenbrainz"]["linked"],
        false
    );
    let before = mock.requests.lock().unwrap().len();
    h.ok("scrobble", &format!("id={sid}")).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(mock.requests.lock().unwrap().len(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn short_tracks_are_not_sent() {
    let mock = mock();
    let h = Harness::new();
    song(&h, 10);
    let sid = first_song(&h).await;
    h.ok("juuzListenbrainzLink", &link_args(&mock)).await;
    h.ok("scrobble", &format!("id={sid}")).await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let posted = mock
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.starts_with("POST"))
        .count();
    assert_eq!(posted, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_token_is_not_linked_and_a_revoked_one_unlinks() {
    let rejecting = mock_server(|_| {
        vec![route(
            "/apis/listenbrainz/1/validate-token",
            200,
            r#"{"code":200,"message":"Token invalid.","valid":false}"#,
        )]
    });
    let h = Harness::new();
    assert_eq!(
        h.call("juuzListenbrainzLink", &link_args(&rejecting)).await["status"],
        "failed"
    );
    assert_eq!(
        h.ok("juuzListenbrainzStatus", "").await["juuzListenbrainz"]["linked"],
        false
    );

    let revoked = mock_server(|_| {
        vec![
            route(
                "/apis/listenbrainz/1/validate-token",
                200,
                r#"{"valid":true}"#,
            ),
            route(
                "/apis/listenbrainz/1/submit-listens",
                401,
                r#"{"code":401,"error":"Invalid authorization token."}"#,
            ),
        ]
    });
    song(&h, 40);
    let sid = first_song(&h).await;
    h.ok("juuzListenbrainzLink", &link_args(&revoked)).await;
    h.ok("scrobble", &format!("id={sid}")).await;
    for _ in 0..100 {
        if h.ok("juuzListenbrainzStatus", "").await["juuzListenbrainz"]["linked"] == false {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("a rejected token must unlink the user");
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_urls_are_refused() {
    let h = Harness::new();
    assert_eq!(
        h.call("juuzListenbrainzLink", "url=ftp%3A%2F%2Fx&token=t")
            .await["status"],
        "failed"
    );
    assert_eq!(
        h.call("juuzListenbrainzLink", "url=http%3A%2F%2Fx").await["status"],
        "failed"
    );
}
