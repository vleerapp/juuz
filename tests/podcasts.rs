mod common;

use common::*;
use serde_json::Value;

fn feed(base: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Test Cast</title><description>about</description>
<item><guid>g1</guid><title>Ep 1</title><pubDate>Mon, 01 Jan 2024 00:00:00 +0000</pubDate><enclosure url="{base}/ep1.mp3" length="9" type="audio/mpeg"/></item>
<item><guid>g2</guid><title>Ep 2</title><pubDate>Tue, 02 Jan 2024 00:00:00 +0000</pubDate><enclosure url="{base}/ep2.mp3" length="9" type="audio/mpeg"/></item>
</channel></rss>"#
    )
}

async fn wait_for<F: Fn(&Value) -> bool>(h: &Harness, f: F) -> Value {
    for _ in 0..100 {
        let v = h.ok("getPodcasts", "").await;
        if f(&v) {
            return v;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("condition not reached: {}", h.ok("getPodcasts", "").await);
}

#[tokio::test(flavor = "multi_thread")]
async fn podcast_lifecycle() {
    let mock = mock_server(|base| {
        vec![
            route("/feed.xml", 200, feed(base)),
            route("/ep1.mp3", 200, "FAKEAUDIO1"),
            route("/ep2.mp3", 200, "FAKEAUDIO2"),
        ]
    });
    let h = Harness::build(None, |c| c.podcasts.allow_private_hosts = true);
    h.ok(
        "createPodcastChannel",
        &format!("url={}/feed.xml", mock.url),
    )
    .await;
    let v = wait_for(&h, |v| v["podcasts"]["channel"][0]["status"] == "completed").await;
    let channel = &v["podcasts"]["channel"][0];
    assert_eq!(channel["title"], "Test Cast");
    let episodes = channel["episode"].as_array().unwrap();
    assert_eq!(episodes.len(), 2);
    assert_eq!(episodes[0]["title"], "Ep 2", "newest first");
    assert_eq!(episodes[0]["status"], "new");
    assert!(episodes[0].get("streamId").is_none());

    let dup = h
        .call(
            "createPodcastChannel",
            &format!("url={}/feed.xml", mock.url),
        )
        .await;
    assert_eq!(dup["status"], "failed");

    let ep = episodes[0]["id"].as_str().unwrap().to_string();
    h.ok("downloadPodcastEpisode", &format!("id={ep}")).await;
    wait_for(&h, |v| {
        v["podcasts"]["channel"][0]["episode"][0]["status"] == "completed"
    })
    .await;
    let got = h.ok("getPodcastEpisode", &format!("id={ep}")).await;
    assert_eq!(got["podcastEpisode"]["streamId"], ep);
    assert_eq!(got["podcastEpisode"]["suffix"], "mp3");

    let streamed = h.get(&format!("/rest/stream?{}&id={ep}", h.auth())).await;
    assert_eq!(streamed.body, b"FAKEAUDIO2");

    let newest = h.ok("getNewestPodcasts", "count=1").await;
    assert_eq!(newest["newestPodcasts"]["episode"][0]["title"], "Ep 2");

    h.ok("refreshPodcasts", "").await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let v = h.ok("getPodcasts", "").await;
    assert_eq!(
        v["podcasts"]["channel"][0]["episode"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "refresh must not duplicate"
    );

    h.ok("deletePodcastEpisode", &format!("id={ep}")).await;
    assert_eq!(
        h.call("stream", &format!("id={ep}")).await["error"]["code"],
        70
    );
    let v = h.ok("getPodcasts", "").await;
    assert_eq!(
        v["podcasts"]["channel"][0]["episode"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let cid = v["podcasts"]["channel"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    h.ok("deletePodcastChannel", &format!("id={cid}")).await;
    assert!(
        h.ok("getPodcasts", "").await["podcasts"]["channel"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn podcast_fetch_refuses_private_hosts_and_needs_role() {
    let h = Harness::new();
    h.ok("createPodcastChannel", "url=http://127.0.0.1:9/feed.xml")
        .await;
    let v = wait_for(&h, |v| v["podcasts"]["channel"][0]["status"] == "error").await;
    assert!(
        v["podcasts"]["channel"][0]["errorMessage"]
            .as_str()
            .unwrap()
            .contains("public"),
        "{v}"
    );
    h.ok("createPodcastChannel", "url=file:///etc/passwd").await;
    wait_for(&h, |v| {
        v["podcasts"]["channel"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["status"] == "error")
    })
    .await;

    h.ok("createUser", "username=plain&password=pw").await;
    let denied = h
        .get("/rest/createPodcastChannel?f=json&u=plain&p=pw&url=http://example.com/x")
        .await
        .json();
    assert_eq!(denied["error"]["code"], 50);
    assert_eq!(
        h.get("/rest/getPodcasts?f=json&u=plain&p=pw").await.json()["status"],
        "ok"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_endpoints_fail_cleanly() {
    let h = Harness::new();
    assert_eq!(h.call("hls", "id=x").await["status"], "failed");
    assert_eq!(h.call("getCaptions", "id=x").await["error"]["code"], 70);
    assert_eq!(h.call("getVideoInfo", "id=x").await["error"]["code"], 70);
    assert_eq!(h.ok("getVideos", "").await["videos"], serde_json::json!({}));
    assert_eq!(
        h.ok("getPodcasts", "").await["podcasts"]["channel"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let root = h.music.path();
    make_track(
        root,
        "A/2000 - X/01 - Findme.wav",
        1,
        &Tags {
            title: Some("Findme"),
            artist: Some("A"),
            album: Some("X"),
            track: Some(1),
            ..Default::default()
        },
    );
    h.scan().await;
    let legacy = h.ok("search", "any=findme").await;
    assert_eq!(legacy["searchResult"]["match"][0]["title"], "Findme");
}
