mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;

fn md5_token(password: &str, salt: &str) -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(password.as_bytes());
    h.update(salt.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn ids_of(song: &serde_json::Value) -> String {
    song["id"].as_str().unwrap().to_string()
}

fn tags<'a>(title: &'a str, artist: &'a str, album: &'a str, track: u32) -> Tags<'a> {
    Tags {
        title: Some(title),
        artist: Some(artist),
        album: Some(album),
        track: Some(track),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ping_and_extensions_in_both_formats() {
    let h = Harness::new();
    let json = h.get(&format!("/rest/ping?{}", h.auth())).await.json();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["openSubsonic"], true);
    assert_eq!(json["type"], "juuz");

    let xml = h
        .get(&format!("/rest/ping.view?u={USER}&p={PASS}"))
        .await
        .text();
    assert!(xml.contains("<subsonic-response"), "{xml}");
    assert!(xml.contains("status=\"ok\""));
    assert!(xml.contains("xmlns=\"http://subsonic.org/restapi\""));

    let ext = h.get("/rest/getOpenSubsonicExtensions?f=json").await.json();
    let names: Vec<_> = ext["openSubsonicExtensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect();
    for n in [
        "apiKeyAuthentication",
        "formPost",
        "openMusicMetadata",
        "juuzSongUpload",
    ] {
        assert!(names.contains(&n.to_string()), "{names:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_matrix() {
    let h = Harness::new();
    let u = |q: &str| format!("/rest/ping?f=json&{q}");

    assert_eq!(h.get(&u("u=admin&p=pw")).await.json()["status"], "ok");
    let enc: String = PASS.bytes().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        h.get(&u(&format!("u=admin&p=enc:{enc}"))).await.json()["status"],
        "ok"
    );
    assert_eq!(
        h.get(&u("u=admin&p=nope")).await.json()["error"]["code"],
        40
    );
    assert_eq!(h.get(&u("u=ghost&p=pw")).await.json()["error"]["code"], 40);
    assert_eq!(h.get(&u("f=json")).await.json()["error"]["code"], 10);

    let salt = "abc123";
    let token = md5_token(PASS, salt);
    assert_eq!(
        h.get(&u(&format!("u=admin&t={token}&s={salt}")))
            .await
            .json()["status"],
        "ok"
    );
    assert_eq!(
        h.get(&u(&format!("u=admin&t=deadbeef&s={salt}")))
            .await
            .json()["error"]["code"],
        40
    );

    h.ok("createUser", "username=newuser&password=secret").await;
    let t = md5_token("secret", salt);
    assert_eq!(
        h.get(&u(&format!("u=newuser&t={t}&s={salt}"))).await.json()["status"],
        "ok",
        "token auth must work for every user without any toggle"
    );
    assert_eq!(h.get(&u("u=newuser&p=secret")).await.json()["status"], "ok");
    h.ok("changePassword", "username=newuser&password=fresh")
        .await;
    assert_eq!(
        h.get(&u(&format!("u=newuser&t={t}&s={salt}"))).await.json()["error"]["code"],
        40,
        "old password token is rejected"
    );
    let t2 = md5_token("fresh", salt);
    assert_eq!(
        h.get(&u(&format!("u=newuser&t={t2}&s={salt}")))
            .await
            .json()["status"],
        "ok"
    );
    h.ok("updateUser", "username=newuser&password=third").await;
    let t3 = md5_token("third", salt);
    assert_eq!(
        h.get(&u(&format!("u=newuser&t={t3}&s={salt}")))
            .await
            .json()["status"],
        "ok"
    );

    let key = h.api_key().await;
    assert_eq!(
        h.get(&u(&format!("apiKey={key}"))).await.json()["status"],
        "ok"
    );
    assert_eq!(
        h.get(&u("apiKey=jz_wrong")).await.json()["error"]["code"],
        44
    );
    assert_eq!(
        h.get(&u(&format!("apiKey={key}&u=admin"))).await.json()["error"]["code"],
        43
    );
    let info = h
        .get(&format!("/rest/tokenInfo?f=json&apiKey={key}"))
        .await
        .json();
    assert_eq!(info["tokenInfo"]["username"], "admin");
}

#[tokio::test(flavor = "multi_thread")]
async fn form_post_params() {
    let h = Harness::new();
    let req = Request::builder()
        .method("POST")
        .uri("/rest/getUser")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(format!(
            "u={USER}&p={PASS}&f=json&username={USER}"
        )))
        .unwrap();
    let v = h.raw(req).await.json();
    assert_eq!(v["status"], "ok");
    assert_eq!(v["user"]["username"], "admin");
}

#[tokio::test(flavor = "multi_thread")]
async fn scan_groups_albums_and_serves_media() {
    let h = Harness::new();
    let root = h.music.path();
    let rock = |title, track| Tags {
        title: Some(title),
        artist: Some("Alpha"),
        album: Some("First"),
        track: Some(track),
        year: Some("2020"),
        genre: Some("Rock"),
        ..Default::default()
    };
    make_track(root, "Alpha/2020 - First/01 - One.wav", 1, &rock("One", 1));
    make_track(root, "Alpha/2020 - First/02 - Two.wav", 2, &rock("Two", 2));
    make_track(
        root,
        "Mixes/Hits/01 - Aaa.wav",
        3,
        &tags("Aaa", "Singer One", "Hits", 1),
    );
    make_track(
        root,
        "Mixes/Hits/02 - Bbb.wav",
        4,
        &tags("Bbb", "Singer Two", "Hits", 2),
    );
    make_track(
        root,
        "Other/Hits/01 - Ccc.wav",
        5,
        &tags("Ccc", "Other Band", "Hits", 1),
    );
    std::fs::write(root.join("Mixes/Hits/not-audio.txt"), "x").unwrap();

    assert_eq!(h.scan().await, 5);
    assert_eq!(
        h.scan().await,
        0,
        "second scan must not re-read unchanged files"
    );

    let albums = h
        .ok("getAlbumList2", "type=alphabeticalByName&size=50")
        .await;
    let list = albums["albumList2"]["album"].as_array().unwrap();
    assert_eq!(list.len(), 3, "{list:?}");
    let mixes = list
        .iter()
        .find(|a| a["name"] == "Hits" && a["songCount"] == 2)
        .expect("compilation album with two songs");
    assert_eq!(mixes["displayArtist"], "Various Artists");
    assert_eq!(mixes["isCompilation"], true);
    let other = list
        .iter()
        .find(|a| a["name"] == "Hits" && a["songCount"] == 1)
        .unwrap();
    assert_eq!(other["displayArtist"], "Other Band");
    assert_ne!(mixes["id"], other["id"]);

    let first = list.iter().find(|a| a["name"] == "First").unwrap();
    assert_eq!(first["year"], 2020);
    let album = h
        .ok("getAlbum", &format!("id={}", first["id"].as_str().unwrap()))
        .await;
    let songs = album["album"]["song"].as_array().unwrap();
    assert_eq!(songs.len(), 2);
    assert_eq!(songs[0]["title"], "One");
    assert_eq!(songs[0]["genre"], "Rock");
    assert_eq!(songs[0]["path"], "Alpha/2020 - First/01 - One.wav");
    assert_eq!(songs[0]["suffix"], "wav");

    let id = ids_of(&songs[0]);
    assert_eq!(id.len(), 24, "ids are cuid-shaped: {id}");
    assert!(!id.contains('-'));

    let artists = h.ok("getArtists", "").await;
    let names: Vec<_> = artists["artists"]["index"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["artist"].as_array().unwrap().iter())
        .map(|a| a["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"Alpha".to_string()));
    assert!(names.contains(&"Various Artists".to_string()));

    let found = h.ok("search3", "query=alp").await;
    assert_eq!(found["searchResult3"]["artist"][0]["name"], "Alpha");
    let found = h.ok("search3", "query=tw").await;
    assert_eq!(found["searchResult3"]["song"][0]["title"], "Two");
    let all = h.ok("search3", "query=&songCount=100").await;
    assert_eq!(all["searchResult3"]["song"].as_array().unwrap().len(), 5);

    let dir = h
        .ok(
            "getMusicDirectory",
            &format!("id={}", first["id"].as_str().unwrap()),
        )
        .await;
    assert_eq!(dir["directory"]["child"].as_array().unwrap().len(), 2);

    let ranged = h
        .raw(
            Request::builder()
                .uri(format!("/rest/stream?{}&id={id}", h.auth()))
                .header("range", "bytes=0-99")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(ranged.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(ranged.body.len(), 100);
    assert_eq!(&ranged.body[..4], b"RIFF");
    let full = h.get(&format!("/rest/stream?{}&id={id}", h.auth())).await;
    assert_eq!(full.status, StatusCode::OK);
    assert!(full.body.len() > 1000);

    let nocover = h.call("getCoverArt", &format!("id={id}")).await;
    assert_eq!(nocover["error"]["code"], 70);

    let genres = h.ok("getGenres", "").await;
    assert_eq!(genres["genres"]["genre"][0]["value"], "Rock");
    let random = h.ok("getRandomSongs", "size=3").await;
    assert_eq!(random["randomSongs"]["song"].as_array().unwrap().len(), 3);

    let xml = h
        .get(&format!(
            "/rest/getAlbum?u={USER}&p={PASS}&id={}",
            first["id"].as_str().unwrap()
        ))
        .await
        .text();
    assert!(xml.contains("<album "), "{xml}");
    assert!(xml.contains("<song "), "{xml}");
    assert!(xml.contains("title=\"One\""));
}

#[tokio::test(flavor = "multi_thread")]
async fn removed_files_are_pruned_and_orphans_cleaned() {
    let h = Harness::new();
    let root = h.music.path();
    let p = make_track(
        root,
        "Solo/2001 - Only/01 - Song.wav",
        7,
        &tags("Song", "Solo", "Only", 1),
    );
    h.scan().await;
    assert_eq!(
        h.ok("getArtists", "").await["artists"]["index"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    std::fs::remove_file(p).unwrap();
    make_track(
        root,
        "Another/2002 - Disc/01 - Keep.wav",
        8,
        &tags("Keep", "Another", "Disc", 1),
    );
    h.scan().await;
    let albums = h.ok("getAlbumList2", "type=newest&size=10").await;
    assert_eq!(albums["albumList2"]["album"].as_array().unwrap().len(), 1);
    assert_eq!(albums["albumList2"]["album"][0]["name"], "Disc");
    let found = h.ok("search3", "query=solo").await;
    assert!(
        found["searchResult3"]["artist"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn annotations_playlists_queue_bookmarks() {
    let h = Harness::new();
    let root = h.music.path();
    make_track(root, "A/2000 - X/01 - S1.wav", 11, &tags("S1", "A", "X", 1));
    make_track(root, "A/2000 - X/02 - S2.wav", 12, &tags("S2", "A", "X", 2));
    h.scan().await;
    let songs = h.ok("search3", "query=s&songCount=10").await;
    let ids: Vec<String> = songs["searchResult3"]["song"]
        .as_array()
        .unwrap()
        .iter()
        .map(ids_of)
        .collect();
    assert_eq!(ids.len(), 2);
    let album_id = songs["searchResult3"]["song"][0]["albumId"]
        .as_str()
        .unwrap()
        .to_string();

    h.ok("star", &format!("id={}&albumId={album_id}", ids[0]))
        .await;
    let starred = h.ok("getStarred2", "").await;
    assert_eq!(starred["starred2"]["song"].as_array().unwrap().len(), 1);
    assert_eq!(starred["starred2"]["album"].as_array().unwrap().len(), 1);
    h.ok("unstar", &format!("id={}", ids[0])).await;
    assert!(
        h.ok("getStarred2", "").await["starred2"]["song"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(h.call("star", "id=nonexistent").await["error"]["code"], 70);

    h.ok("setRating", &format!("id={}&rating=4", ids[1])).await;
    assert_eq!(
        h.ok("getSong", &format!("id={}", ids[1])).await["song"]["userRating"],
        4
    );
    h.ok("setRating", &format!("id={}&rating=0", ids[1])).await;
    assert!(
        h.ok("getSong", &format!("id={}", ids[1])).await["song"]
            .get("userRating")
            .is_none()
    );

    h.ok("scrobble", &format!("id={}&submission=false", ids[0]))
        .await;
    assert_eq!(
        h.ok("getNowPlaying", "").await["nowPlaying"]["entry"][0]["username"],
        "admin"
    );
    h.ok("scrobble", &format!("id={}&time=1700000000000", ids[0]))
        .await;
    h.ok("scrobble", &format!("id={}", ids[0])).await;
    let s = h.ok("getSong", &format!("id={}", ids[0])).await;
    assert_eq!(s["song"]["playCount"], 2);
    assert!(
        h.ok("getNowPlaying", "").await["nowPlaying"]["entry"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let freq = h.ok("getAlbumList2", "type=frequent").await;
    assert_eq!(freq["albumList2"]["album"].as_array().unwrap().len(), 1);

    let created = h
        .ok(
            "createPlaylist",
            &format!("name=Mix&songId={}&songId={}", ids[0], ids[1]),
        )
        .await;
    let pid = created["playlist"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["playlist"]["songCount"], 2);
    h.ok(
        "updatePlaylist",
        &format!("playlistId={pid}&songIndexToRemove=0&comment=hi&public=true"),
    )
    .await;
    let pl = h.ok("getPlaylist", &format!("id={pid}")).await;
    assert_eq!(pl["playlist"]["entry"].as_array().unwrap().len(), 1);
    assert_eq!(pl["playlist"]["entry"][0]["id"], ids[1]);
    assert_eq!(pl["playlist"]["public"], true);
    h.ok(
        "updatePlaylist",
        &format!("playlistId={pid}&songIdToAdd={}", ids[0]),
    )
    .await;
    assert_eq!(
        h.ok("getPlaylist", &format!("id={pid}")).await["playlist"]["songCount"],
        2
    );
    assert_eq!(
        h.ok("getPlaylists", "").await["playlists"]["playlist"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let cover = h.call("getCoverArt", &format!("id={pid}")).await;
    assert_eq!(cover["error"]["code"], 70);
    h.ok("deletePlaylist", &format!("id={pid}")).await;
    assert_eq!(
        h.call("getPlaylist", &format!("id={pid}")).await["error"]["code"],
        70
    );

    h.ok(
        "savePlayQueue",
        &format!(
            "id={}&id={}&current={}&position=1234",
            ids[0], ids[1], ids[1]
        ),
    )
    .await;
    let q = h.ok("getPlayQueue", "").await;
    assert_eq!(q["playQueue"]["current"], ids[1]);
    assert_eq!(q["playQueue"]["position"], 1234);
    assert_eq!(q["playQueue"]["entry"].as_array().unwrap().len(), 2);
    assert_eq!(q["playQueue"]["changedBy"], "test");
    h.ok(
        "savePlayQueueByIndex",
        &format!("id={}&id={}&currentIndex=1&position=9", ids[0], ids[1]),
    )
    .await;
    let q = h.ok("getPlayQueueByIndex", "").await;
    assert_eq!(q["playQueueByIndex"]["currentIndex"], 1);

    h.ok(
        "createBookmark",
        &format!("id={}&position=5000&comment=resume", ids[0]),
    )
    .await;
    let b = h.ok("getBookmarks", "").await;
    assert_eq!(b["bookmarks"]["bookmark"][0]["position"], 5000);
    assert_eq!(b["bookmarks"]["bookmark"][0]["entry"]["id"], ids[0]);
    h.ok("deleteBookmark", &format!("id={}", ids[0])).await;
    assert!(
        h.ok("getBookmarks", "").await["bookmarks"]["bookmark"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shares_radio_chat_and_user_management() {
    let h = Harness::new();
    let root = h.music.path();
    make_track(root, "A/2000 - X/01 - S1.wav", 21, &tags("S1", "A", "X", 1));
    h.scan().await;
    let sid = h.ok("search3", "query=s1").await["searchResult3"]["song"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let share = h
        .ok("createShare", &format!("id={sid}&description=hello"))
        .await;
    let share_id = share["shares"]["share"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        share["shares"]["share"][0]["entry"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let public = h.get(&format!("/share/{share_id}")).await;
    assert_eq!(public.status, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&public.body).unwrap();
    assert_eq!(body["entries"][0]["title"], "S1");
    let stream = h.get(&format!("/share/{share_id}/stream/{sid}")).await;
    assert_eq!(stream.status, StatusCode::OK);
    assert_eq!(&stream.body[..4], b"RIFF");
    assert_eq!(h.get("/share/nope").await.status, StatusCode::NOT_FOUND);
    h.ok("deleteShare", &format!("id={share_id}")).await;
    assert_eq!(
        h.get(&format!("/share/{share_id}")).await.status,
        StatusCode::NOT_FOUND
    );

    h.ok(
        "createInternetRadioStation",
        "name=Jazz&streamUrl=http://example.com/s&homepageUrl=http://example.com",
    )
    .await;
    let radios = h.ok("getInternetRadioStations", "").await;
    let rid = radios["internetRadioStations"]["internetRadioStation"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    h.ok(
        "updateInternetRadioStation",
        &format!("id={rid}&name=Jazz2&streamUrl=http://example.com/t"),
    )
    .await;
    assert_eq!(
        h.ok("getInternetRadioStations", "").await["internetRadioStations"]["internetRadioStation"]
            [0]["name"],
        "Jazz2"
    );
    h.ok("deleteInternetRadioStation", &format!("id={rid}"))
        .await;

    h.ok("addChatMessage", "message=hi%20there").await;
    let chat = h.ok("getChatMessages", "").await;
    assert_eq!(
        chat["chatMessages"]["chatMessage"][0]["message"],
        "hi there"
    );

    h.ok(
        "createUser",
        "username=bob&password=pw2&uploadRole=true&shareRole=true",
    )
    .await;
    let bob = h.get("/rest/getUser?f=json&u=bob&p=pw2").await.json();
    assert_eq!(bob["user"]["uploadRole"], true);
    assert_eq!(bob["user"]["adminRole"], false);
    let denied = h.get("/rest/getUsers?f=json&u=bob&p=pw2").await.json();
    assert_eq!(denied["error"]["code"], 50);
    h.ok("changePassword", "username=bob&password=newpw").await;
    assert_eq!(
        h.get("/rest/ping?f=json&u=bob&p=newpw").await.json()["status"],
        "ok"
    );
    assert_eq!(
        h.get("/rest/ping?f=json&u=bob&p=pw2").await.json()["error"]["code"],
        40
    );
    h.ok("deleteUser", "username=bob").await;
    assert_eq!(
        h.get("/rest/ping?f=json&u=bob&p=newpw").await.json()["error"]["code"],
        40
    );

    let status = h.ok("getScanStatus", "").await;
    assert_eq!(status["scanStatus"]["scanning"], false);
    assert_eq!(status["scanStatus"]["count"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn lyrics_from_sidecar() {
    let h = Harness::new();
    let root = h.music.path();
    let p = make_track(root, "A/2000 - X/01 - S1.wav", 31, &tags("S1", "A", "X", 1));
    std::fs::write(
        p.with_extension("lrc"),
        "[00:01.50]first\n[00:03.00]second\n",
    )
    .unwrap();
    h.scan().await;
    let sid = h.ok("search3", "query=s1").await["searchResult3"]["song"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let l = h.ok("getLyricsBySongId", &format!("id={sid}")).await;
    let s = &l["lyricsList"]["structuredLyrics"][0];
    assert_eq!(s["synced"], true);
    assert_eq!(s["line"][0]["start"], 1500);
    assert_eq!(s["line"][1]["value"], "second");
    let plain = h.ok("getLyrics", "title=S1&artist=A").await;
    assert_eq!(plain["lyrics"]["value"], "first\nsecond");
}

#[tokio::test(flavor = "multi_thread")]
async fn watcher_move_keeps_user_data() {
    let h = Harness::new();
    let root = h.root();
    let old = make_track(
        &root,
        "Band/2010 - Record/01 - Song.wav",
        51,
        &tags("Song", "Band", "Record", 1),
    );
    h.scan().await;
    let sid = h.ok("search3", "query=song").await["searchResult3"]["song"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    h.ok("star", &format!("id={sid}")).await;
    h.ok("setRating", &format!("id={sid}&rating=5")).await;
    h.ok("scrobble", &format!("id={sid}")).await;
    let pl = h
        .ok("createPlaylist", &format!("name=P&songId={sid}"))
        .await["playlist"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    std::fs::create_dir_all(root.join("Moved Elsewhere")).unwrap();
    std::fs::rename(
        root.join("Band/2010 - Record"),
        root.join("Moved Elsewhere/Renamed"),
    )
    .unwrap();
    let new = root.join("Moved Elsewhere/Renamed/01 - Song.wav");
    let scanner = h.server.state.scanner.clone();
    let set = std::collections::HashSet::from([old.clone(), new.clone()]);
    tokio::task::spawn_blocking(move || juuz::scanner::watch::process_paths(&scanner, set))
        .await
        .unwrap();

    let songs = h.ok("search3", "query=song").await["searchResult3"]["song"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(songs.len(), 1, "{songs:?}");
    assert_eq!(songs[0]["path"], "Moved Elsewhere/Renamed/01 - Song.wav");
    assert!(
        songs[0]["starred"].is_string(),
        "star lost on move: {}",
        songs[0]
    );
    assert_eq!(songs[0]["userRating"], 5);
    assert_eq!(songs[0]["playCount"], 1);
    let playlist = h.ok("getPlaylist", &format!("id={pl}")).await;
    assert_eq!(playlist["playlist"]["songCount"], 1);

    let h2 = Harness::new();
    let root2 = h2.root();
    let old2 = make_track(&root2, "A/2000 - X/01 - S.wav", 52, &tags("S", "A", "X", 1));
    h2.scan().await;
    let sid = h2.ok("search3", "query=s").await["searchResult3"]["song"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    h2.ok("star", &format!("id={sid}")).await;
    std::fs::rename(&old2, root2.join("A/2000 - X/renamed file.wav")).unwrap();
    h2.scan().await;
    let moved = h2.ok("search3", "query=s").await["searchResult3"]["song"][0].clone();
    assert!(
        moved["starred"].is_string(),
        "star lost across full-scan move: {moved}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unauthenticated_oversized_fields_are_rejected_without_buffering() {
    let h = Harness::new();
    let big = "x".repeat(2 * 1024 * 1024);
    let started = std::time::Instant::now();
    let r = h
        .upload_raw("", &[("junk", &big)], "x.wav", &wav_bytes(1, 300))
        .await
        .json();
    assert_eq!(r["status"], "failed", "{r}");
    assert!(started.elapsed().as_secs() < 5);
    assert!(!h.root().join(".juuz-tmp").exists() || count_files(&h.root().join(".juuz-tmp")) == 0);

    let many: Vec<(String, String)> = (0..100)
        .map(|i| (format!("f{i}"), "v".to_string()))
        .collect();
    let fields: Vec<(&str, &str)> = many.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let r = h
        .upload_raw("", &fields, "x.wav", &wav_bytes(2, 300))
        .await
        .json();
    assert_eq!(r["status"], "failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn cors_headers_are_present() {
    let h = Harness::new();
    let req = Request::builder()
        .uri(format!("/rest/ping?{}", h.auth()))
        .header("origin", "https://client.example")
        .body(Body::empty())
        .unwrap();
    let resp = h.raw(req).await;
    assert_eq!(resp.headers["access-control-allow-origin"], "*");
}

#[tokio::test(flavor = "multi_thread")]
async fn users_without_a_stored_secret_upgrade_on_first_password_login() {
    let h = Harness::new();
    let hash = juuz::auth::hash_password("oldpass").unwrap();
    {
        let conn = h.server.state.db.conn().unwrap();
        juuz::db::users::create(&conn, "oldschool", &hash, None, false).unwrap();
    }
    let salt = "s1";
    let token = md5_token("oldpass", salt);
    let before = h
        .get(&format!("/rest/ping?f=json&u=oldschool&t={token}&s={salt}"))
        .await
        .json();
    assert_eq!(before["error"]["code"], 41, "{before}");
    assert_eq!(
        h.get("/rest/ping?f=json&u=oldschool&p=wrong").await.json()["error"]["code"],
        40
    );
    assert_eq!(
        h.get("/rest/ping?f=json&u=oldschool&p=oldpass")
            .await
            .json()["status"],
        "ok"
    );
    let after = h
        .get(&format!("/rest/ping?f=json&u=oldschool&t={token}&s={salt}"))
        .await
        .json();
    assert_eq!(
        after["status"], "ok",
        "token auth works after one password login"
    );
    assert_eq!(
        h.get("/rest/ping?f=json&u=oldschool&p=wrong").await.json()["error"]["code"],
        40
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn music_folder_filter_works_on_every_endpoint_that_takes_it() {
    let h = Harness::new();
    let root = h.music.path();
    make_track(
        root,
        "Alpha/2020 - First/01 - One.wav",
        61,
        &tags("One", "Alpha", "First", 1),
    );
    make_track(
        root,
        "Alpha/2020 - First/02 - Two.wav",
        62,
        &tags("Two", "Alpha", "First", 2),
    );
    h.scan().await;
    let folder = h.ok("getMusicFolders", "").await["musicFolders"]["musicFolder"][0]["id"]
        .as_i64()
        .unwrap();
    let q = format!("musicFolderId={folder}");

    let found = h.ok("search3", &format!("query=on&{q}")).await["searchResult3"].clone();
    assert_eq!(found["song"][0]["title"], "One");
    assert_eq!(found["album"].as_array().unwrap().len(), 0);
    let found = h.ok("search3", &format!("query=first&{q}")).await["searchResult3"].clone();
    assert_eq!(found["album"][0]["name"], "First");
    let all = h.ok("search3", &format!("query=&{q}&songCount=10")).await["searchResult3"].clone();
    assert_eq!(all["song"].as_array().unwrap().len(), 2);
    h.ok("search2", &format!("query=one&{q}")).await;

    for (endpoint, extra) in [
        ("getIndexes", ""),
        ("getArtists", ""),
        ("getAlbumList2", "type=newest"),
        ("getAlbumList2", "type=alphabeticalByName"),
        ("getAlbumList2", "type=random"),
        ("getAlbumList", "type=newest"),
        ("getRandomSongs", "size=5"),
        ("getSongsByGenre", "genre=Rock"),
        ("getStarred", ""),
        ("getStarred2", ""),
    ] {
        let v = h.call(endpoint, &format!("{q}&{extra}")).await;
        assert_eq!(
            v["status"], "ok",
            "{endpoint} {extra} with a folder filter: {v}"
        );
    }
    let listed = h.ok("getAlbumList2", &format!("type=newest&{q}")).await;
    assert_eq!(listed["albumList2"]["album"].as_array().unwrap().len(), 1);
}
