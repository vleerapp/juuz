mod common;

use common::*;

const HIT: &str = r#"{"id":1,"trackName":"Temple","artistName":"Lifelikemoviee","albumName":"X","duration":1,"instrumental":false,"plainLyrics":"hello\nworld","syncedLyrics":"[00:01.00]hello\n[00:02.50]world"}"#;

fn lyrics_url(url: &str) -> String {
    format!("{url}/api")
}

async fn song_id(h: &Harness, title: &str) -> String {
    h.ok("search3", &format!("query={title}")).await["searchResult3"]["song"][0]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn track(h: &Harness) -> std::path::PathBuf {
    make_track(
        h.music.path(),
        "Lifelikemoviee/2025 - X/01 - Temple.wav",
        1,
        &Tags {
            title: Some("Temple"),
            artist: Some("Lifelikemoviee"),
            album: Some("X"),
            track: Some(1),
            ..Default::default()
        },
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_lyrics_are_fetched_from_lrclib_and_cached() {
    let mock = mock_server(|_| vec![route("/api/get", 200, HIT)]);
    let url = lyrics_url(&mock.url);
    let h = Harness::build(None, move |c| c.lrclib_url = url);
    track(&h);
    h.scan().await;
    let id = song_id(&h, "temple").await;

    let v = h.ok("getLyricsBySongId", &format!("id={id}")).await;
    let lyrics = &v["lyricsList"]["structuredLyrics"][0];
    assert_eq!(lyrics["synced"], true);
    assert_eq!(lyrics["line"][0]["start"], 1000);
    assert_eq!(lyrics["line"][1]["value"], "world");
    assert_eq!(lyrics["displayTitle"], "Temple");
    let hits = mock.hits.lock().unwrap().len();
    assert!(hits >= 1);
    let first = mock.hits.lock().unwrap()[0].clone();
    assert!(
        first.contains("track_name=Temple") && first.contains("artist_name=Lifelikemoviee"),
        "{first}"
    );

    let again = h.ok("getLyricsBySongId", &format!("id={id}")).await;
    assert_eq!(again["lyricsList"], v["lyricsList"]);
    assert_eq!(
        mock.hits.lock().unwrap().len(),
        hits,
        "second request must come from the cache"
    );

    let legacy = h
        .ok("getLyrics", "title=Temple&artist=Lifelikemoviee")
        .await;
    assert_eq!(legacy["lyrics"]["value"], "hello\nworld");
}

#[tokio::test(flavor = "multi_thread")]
async fn lookups_that_find_nothing_are_remembered() {
    let mock = mock_server(|_| {
        vec![
            route("/api/get", 404, r#"{"statusCode":404}"#),
            route("/api/search", 200, "[]"),
        ]
    });
    let url = lyrics_url(&mock.url);
    let h = Harness::build(None, move |c| c.lrclib_url = url);
    track(&h);
    h.scan().await;
    let id = song_id(&h, "temple").await;
    let v = h.ok("getLyricsBySongId", &format!("id={id}")).await;
    assert!(
        v["lyricsList"]["structuredLyrics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let hits = mock.hits.lock().unwrap().len();
    h.ok("getLyricsBySongId", &format!("id={id}")).await;
    assert_eq!(
        mock.hits.lock().unwrap().len(),
        hits,
        "misses are cached too"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn local_synced_lyrics_never_hit_the_network() {
    let mock = mock_server(|_| vec![route("/api/get", 200, HIT)]);
    let url = lyrics_url(&mock.url);
    let h = Harness::build(None, move |c| c.lrclib_url = url);
    let p = track(&h);
    std::fs::write(p.with_extension("lrc"), "[00:05.00]local\n").unwrap();
    h.scan().await;
    let id = song_id(&h, "temple").await;
    let v = h.ok("getLyricsBySongId", &format!("id={id}")).await;
    assert_eq!(
        v["lyricsList"]["structuredLyrics"][0]["line"][0]["value"],
        "local"
    );
    assert!(mock.hits.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn lrclib_outage_falls_back_to_local_plain_text_and_retries_later() {
    let h = Harness::build(None, |c| c.lrclib_url = "http://127.0.0.1:9/api".into());
    let p = track(&h);
    std::fs::write(p.with_extension("txt"), "just words\n").unwrap();
    h.scan().await;
    let id = song_id(&h, "temple").await;
    let v = h.ok("getLyricsBySongId", &format!("id={id}")).await;
    let lyrics = &v["lyricsList"]["structuredLyrics"][0];
    assert_eq!(lyrics["synced"], false);
    assert_eq!(lyrics["line"][0]["value"], "just words");
    let conn = h.server.state.db.conn().unwrap();
    let cached: i64 = conn
        .query_row("SELECT COUNT(*) FROM lyrics", [], |r| r.get(0))
        .unwrap();
    assert_eq!(cached, 0, "network errors must not be cached as misses");
}
