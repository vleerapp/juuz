mod common;

use common::*;
use juuz::omm;
use serde_json::Value;

fn result(v: &Value) -> &Value {
    assert_eq!(v["status"], "ok", "{v}");
    &v["juuzUploadResult"]
}

const SONG_ID: &str = "omm:song:1234567890";

#[tokio::test(flavor = "multi_thread")]
async fn upload_with_omm_id_fills_missing_tags_and_stores_ids() {
    let mock = mock_omm(vec![route("/catalog/omm:song:1234567890", 200, OMM_SONG)]);
    let h = Harness::with_omm(Some(mock.url.clone()));
    let key = h.api_key().await;
    let v = h
        .upload(
            &format!("apiKey={key}&ommId={SONG_ID}"),
            "raw.wav",
            &wav_bytes(1, 500),
        )
        .await;
    let song = &result(&v)["song"];
    assert_eq!(song["title"], "Digital Love");
    assert_eq!(song["artist"], "Daft Punk");
    assert_eq!(song["album"], "Discovery");
    assert_eq!(song["track"], 3);
    assert_eq!(song["year"], 2001);
    assert_eq!(song["genre"], "Electronic");
    assert_eq!(
        song["path"],
        "Daft Punk/2001 - Discovery/03 - Digital Love.wav"
    );
    assert_eq!(song["ommId"], SONG_ID);

    let album = h
        .ok(
            "getAlbum",
            &format!("id={}", song["albumId"].as_str().unwrap()),
        )
        .await;
    assert_eq!(album["album"]["ommId"], "omm:album:2345678901");
    assert_eq!(album["album"]["recordLabels"][0]["name"], "Virgin");
    let artist = h
        .ok(
            "getArtist",
            &format!("id={}", song["artistId"].as_str().unwrap()),
        )
        .await;
    assert_eq!(artist["artist"]["ommId"], "omm:artist:3456789012");
}

#[tokio::test(flavor = "multi_thread")]
async fn omm_only_fills_gaps_unless_preferred() {
    let mock = mock_omm(vec![route("/catalog/omm:song:1234567890", 200, OMM_SONG)]);
    let h = Harness::with_omm(Some(mock.url.clone()));
    let key = h.api_key().await;
    let tagged = tagged_wav_bytes(
        2,
        &Tags {
            title: Some("dl (rip)"),
            artist: Some("daft punk"),
            album: Some("Discovery"),
            track: Some(3),
            ..Default::default()
        },
    );
    let v = h
        .upload(&format!("apiKey={key}&ommId={SONG_ID}"), "x.wav", &tagged)
        .await;
    assert_eq!(
        result(&v)["song"]["title"],
        "dl (rip)",
        "file tags win without preferOmm"
    );
    assert_eq!(result(&v)["song"]["year"], 2001, "empty year is filled");

    let tagged = tagged_wav_bytes(
        3,
        &Tags {
            title: Some("dl (rip)"),
            artist: Some("daft punk"),
            album: Some("Discovery"),
            track: Some(3),
            ..Default::default()
        },
    );
    let v = h
        .upload(
            &format!("apiKey={key}&ommId={SONG_ID}&preferOmm=true"),
            "x.wav",
            &tagged,
        )
        .await;
    assert_eq!(result(&v)["song"]["title"], "Digital Love");
    assert_eq!(result(&v)["song"]["artist"], "Daft Punk");

    let tagged = tagged_wav_bytes(
        4,
        &Tags {
            title: Some("x"),
            ..Default::default()
        },
    );
    let v = h
        .upload(
            &format!("apiKey={key}&ommId={SONG_ID}&preferOmm=true&title=Mine"),
            "x.wav",
            &tagged,
        )
        .await;
    assert_eq!(
        result(&v)["song"]["title"],
        "Mine",
        "explicit overrides beat omm"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_survives_omm_outage_and_keeps_client_resolved_id() {
    let h = Harness::with_omm(Some("http://127.0.0.1:9".into()));
    let key = h.api_key().await;
    let started = std::time::Instant::now();
    let v = h
        .upload(
            &format!("apiKey={key}&ommId={SONG_ID}&relativePath=Daft%20Punk%5CDiscovery%5C03%20-%20Digital%20Love.wav"),
            "x.wav",
            &wav_bytes(5, 500),
        )
        .await;
    assert!(started.elapsed().as_secs() < 10);
    let song = &result(&v)["song"];
    assert_eq!(song["title"], "Digital Love");
    assert_eq!(
        song["ommId"], SONG_ID,
        "client supplied id is kept even when the lookup fails"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_omm_ids_are_ignored() {
    let h = Harness::new();
    let key = h.api_key().await;
    let v = h
        .upload(
            &format!("apiKey={key}&ommId=garbage&title=T&artist=A&album=B"),
            "x.wav",
            &wav_bytes(6, 400),
        )
        .await;
    assert!(result(&v)["song"].get("ommId").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_resolves_and_rejects_low_confidence_matches() {
    let mock = mock_omm(vec![
        route("/identify/song?name=Digital", 200, OMM_SONG),
        route("/identify/song?name=Unrelated", 200, OMM_SONG),
        route(
            "/identify/artist?name=Daft",
            200,
            r#"{"data":{"id":"omm:artist:3456789012","type":"artist","attributes":{"name":"Daft Punk"}}}"#,
        ),
        route(
            "/identify/artist",
            200,
            r#"{"data":{"id":"omm:artist:9456789012","type":"artist","attributes":{"name":"Completely Other"}}}"#,
        ),
        route(
            "/identify/album",
            200,
            r#"{"data":{"id":"omm:album:2345678901","type":"album","attributes":{"name":"Discovery","trackCount":14,"artistNames":["Daft Punk"]}}}"#,
        ),
    ]);
    let h = Harness::with_omm(Some(mock.url.clone()));
    make_track(
        h.music.path(),
        "Daft Punk/2001 - Discovery/03 - Digital Love.wav",
        1,
        &Tags {
            title: Some("Digital Love"),
            artist: Some("Daft Punk"),
            album: Some("Discovery"),
            track: Some(3),
            ..Default::default()
        },
    );
    make_track(
        h.music.path(),
        "Nobody/2020 - Nothing/01 - Unrelated Song.wav",
        2,
        &Tags {
            title: Some("Unrelated Song"),
            artist: Some("Nobody"),
            album: Some("Nothing"),
            track: Some(1),
            ..Default::default()
        },
    );
    h.scan().await;

    let client = h.server.state.omm.clone().unwrap();
    let db = h.server.state.db.clone();
    let art_dir = h.data.path().join("omm-art");
    tokio::task::spawn_blocking(move || {
        let mut rounds = 0;
        while omm::pass(&db, &client, &art_dir).unwrap() {
            rounds += 1;
            assert!(rounds < 20, "worker did not converge");
        }
    })
    .await
    .unwrap();

    let digital = h.ok("search3", "query=digital").await;
    let song = &digital["searchResult3"]["song"][0];
    assert_eq!(song["ommId"], SONG_ID);
    assert_eq!(song["isrc"][0], "FRZ390100010");
    let unrelated = h.ok("search3", "query=unrelated").await;
    assert!(
        unrelated["searchResult3"]["song"][0].get("ommId").is_none(),
        "{unrelated}"
    );

    let artists = h.ok("search3", "query=daft").await;
    assert_eq!(
        artists["searchResult3"]["artist"][0]["ommId"],
        "omm:artist:3456789012"
    );
    let nobody = h.ok("search3", "query=nobody").await;
    assert!(nobody["searchResult3"]["artist"][0].get("ommId").is_none());

    let conn = h.server.state.db.conn().unwrap();
    let retry: Option<i64> = conn
        .query_row(
            "SELECT omm_retry_at FROM songs WHERE title = 'Unrelated Song'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        retry.is_some(),
        "unresolved items back off instead of being re-queried every pass"
    );
    let hits_before = mock.hits.lock().unwrap().len();
    let client = h.server.state.omm.clone().unwrap();
    let db = h.server.state.db.clone();
    let art_dir = h.data.path().join("omm-art");
    let more = tokio::task::spawn_blocking(move || omm::pass(&db, &client, &art_dir).unwrap())
        .await
        .unwrap();
    assert!(!more);
    assert_eq!(mock.hits.lock().unwrap().len(), hits_before);
}

#[tokio::test(flavor = "multi_thread")]
async fn client_honors_retry_after_on_rate_limit() {
    let mock = mock_omm(vec![
        route_once("/identify/artist", 429, r#"{"error":{"status":429}}"#),
        route(
            "/identify/artist",
            200,
            r#"{"data":{"id":"omm:artist:3456789012","type":"artist","attributes":{"name":"Daft Punk"}}}"#,
        ),
    ]);
    let cfg = juuz::config::OmmConfig {
        enabled: true,
        base_url: mock.url.clone(),
        timeout_secs: 3,
        upload_timeout_secs: 3,
    };
    let started = std::time::Instant::now();
    let found =
        tokio::task::spawn_blocking(move || omm::Client::new(&cfg).identify_artist("Daft Punk"))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(found.unwrap().id, "omm:artist:3456789012");
    assert!(
        started.elapsed().as_millis() >= 900,
        "must wait for Retry-After"
    );
    assert_eq!(mock.hits.lock().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn server_works_without_omm() {
    let h = Harness::new();
    assert!(h.server.state.omm.is_none());
    make_track(
        h.music.path(),
        "A/2000 - B/01 - C.wav",
        1,
        &Tags {
            title: Some("C"),
            artist: Some("A"),
            album: Some("B"),
            track: Some(1),
            ..Default::default()
        },
    );
    h.scan().await;
    let v = h.ok("search3", "query=c").await;
    assert!(v["searchResult3"]["song"][0].get("ommId").is_none());
}

fn png_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    image::RgbImage::from_pixel(32, 32, image::Rgb([20, 120, 220]))
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn album_match_seeds_songs_and_fills_missing_artwork() {
    let png = png_bytes();
    let mock = mock_server(|base| {
        vec![
            route(
                "/identify/song",
                404,
                r#"{"error":{"status":404,"message":"nope"}}"#,
            ),
            route(
                "/identify/album",
                200,
                format!(
                    r#"{{"data":{{"id":"omm:album:2345678901","type":"album","attributes":{{"name":"Discovery","trackCount":14,"artistNames":["Daft Punk"],"upc":"724384960650","label":"Virgin","genres":["Electronic"],"releaseDate":"2001-03-12","artworkUrl":"{base}/img/{{w}}x{{h}}.png"}}}}}}"#
                ),
            ),
            route(
                "/identify/artist",
                200,
                format!(
                    r#"{{"data":{{"id":"omm:artist:3456789012","type":"artist","attributes":{{"name":"Daft Punk","artworkUrl":"{base}/img/{{w}}x{{h}}.png"}}}}}}"#
                ),
            ),
            route("/img/", 200, png.clone()),
        ]
    });
    let h = Harness::with_omm(Some(mock.url.clone()));
    make_track(
        h.music.path(),
        "Daft Punk/Discovery/01 - Obscure Edit.wav",
        1,
        &Tags {
            title: Some("Obscure Edit"),
            artist: Some("Daft Punk"),
            album: Some("Discovery"),
            track: Some(1),
            ..Default::default()
        },
    );
    h.scan().await;
    let before = h.ok("search3", "query=obscure").await["searchResult3"]["song"][0].clone();
    assert!(
        before.get("genre").is_none() && before.get("year").is_none(),
        "{before}"
    );
    assert!(before.get("coverArt").is_none());

    let (client, db, art_dir) = (
        h.server.state.omm.clone().unwrap(),
        h.server.state.db.clone(),
        h.data.path().join("omm-art"),
    );
    tokio::task::spawn_blocking(move || {
        let mut rounds = 0;
        while omm::pass(&db, &client, &art_dir).unwrap() {
            rounds += 1;
            assert!(rounds < 20);
        }
    })
    .await
    .unwrap();

    let song = h.ok("search3", "query=obscure").await["searchResult3"]["song"][0].clone();
    assert_eq!(
        song["genre"], "Electronic",
        "album genres seed songs the song lookup missed"
    );
    assert_eq!(song["year"], 2001);
    assert!(
        song.get("ommId").is_none(),
        "the song itself stays unmatched"
    );

    let album_id = song["albumId"].as_str().unwrap();
    let album = h.ok("getAlbum", &format!("id={album_id}")).await["album"].clone();
    assert_eq!(album["ommId"], "omm:album:2345678901");
    assert_eq!(album["coverArt"], album_id);
    assert_eq!(album["recordLabels"][0]["name"], "Virgin");

    let cover = h
        .get(&format!("/rest/getCoverArt?{}&id={album_id}", h.auth()))
        .await;
    assert_eq!(cover.headers["content-type"], "image/png");
    assert_eq!(cover.body, png);
    let artist_id = song["artistId"].as_str().unwrap();
    let artist = h.ok("getArtist", &format!("id={artist_id}")).await["artist"].clone();
    assert_eq!(artist["coverArt"], artist_id);
    let image = h
        .get(&format!("/rest/getCoverArt?{}&id={artist_id}", h.auth()))
        .await;
    assert_eq!(image.body, png);

    let scanner = h.server.state.scanner.clone();
    tokio::task::spawn_blocking(move || scanner.scan_blocking(true).unwrap())
        .await
        .unwrap();
    let again = h
        .get(&format!("/rest/getCoverArt?{}&id={album_id}", h.auth()))
        .await;
    assert_eq!(again.body, png, "a rescan must not drop seeded artwork");

    let local = make_track(
        h.music.path(),
        "Daft Punk/Discovery/02 - Other.wav",
        2,
        &Tags {
            title: Some("Other"),
            artist: Some("Daft Punk"),
            album: Some("Discovery"),
            track: Some(2),
            ..Default::default()
        },
    );
    std::fs::write(local.parent().unwrap().join("cover.png"), png_bytes_other()).unwrap();
    let scanner = h.server.state.scanner.clone();
    tokio::task::spawn_blocking(move || scanner.scan_blocking(true).unwrap())
        .await
        .unwrap();
    let local_cover = h
        .get(&format!("/rest/getCoverArt?{}&id={album_id}", h.auth()))
        .await;
    assert_eq!(
        local_cover.body,
        png_bytes_other(),
        "local artwork wins over seeded artwork"
    );
}

fn png_bytes_other() -> Vec<u8> {
    let mut out = Vec::new();
    image::RgbImage::from_pixel(16, 16, image::Rgb([250, 10, 10]))
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}
