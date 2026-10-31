mod common;

use common::*;
use juuz::db::{libraries, users};
use serde_json::Value;

fn tags<'a>(title: &'a str, artist: &'a str, album: &'a str) -> Tags<'a> {
    Tags {
        title: Some(title),
        artist: Some(artist),
        album: Some(album),
        track: Some(1),
        genre: Some("Rock"),
        ..Default::default()
    }
}

struct Setup {
    h: Harness,
    _family: tempfile::TempDir,
    family_id: i64,
    own_song: String,
    family_song: String,
    family_album: String,
    family_artist: String,
}

async fn as_user(h: &Harness, user: &str, pass: &str, endpoint: &str, query: &str) -> Value {
    h.get(&format!(
        "/rest/{endpoint}?u={user}&p={pass}&f=json&c=test&{query}"
    ))
    .await
    .json()
}

async fn setup() -> Setup {
    let h = Harness::new();
    let family = tempfile::tempdir().unwrap();
    make_track(
        h.music.path(),
        "Alpha/First/01 - One.wav",
        71,
        &tags("One", "Alpha", "First"),
    );
    make_track(
        family.path(),
        "Beta/Second/01 - Two.wav",
        72,
        &tags("Two", "Beta", "Second"),
    );
    let path = family.path().to_path_buf();
    let family_id = h
        .server
        .state
        .db
        .run(move |c| Ok(libraries::create(c, "Family", &path, true).unwrap()))
        .await
        .unwrap();
    h.server.state.db.refresh_roots().unwrap();
    h.scan().await;

    let find = |q: &str| {
        let q = q.to_string();
        let h = &h;
        async move { h.ok("search3", &format!("query={q}")).await["searchResult3"].clone() }
    };
    let own = find("one").await;
    let fam = find("two").await;
    let fam_album = find("second").await;
    let fam_artist = find("beta").await;
    Setup {
        own_song: own["song"][0]["id"].as_str().unwrap().into(),
        family_song: fam["song"][0]["id"].as_str().unwrap().into(),
        family_album: fam_album["album"][0]["id"].as_str().unwrap().into(),
        family_artist: fam_artist["artist"][0]["id"].as_str().unwrap().into(),
        h,
        _family: family,
        family_id,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_sees_every_library_and_each_song_keeps_its_own() {
    let s = setup().await;
    let folders = s.h.ok("getMusicFolders", "").await["musicFolders"]["musicFolder"].clone();
    assert_eq!(folders.as_array().unwrap().len(), 2);

    let all = s.h.ok("search3", "query=&songCount=10").await["searchResult3"].clone();
    assert_eq!(all["song"].as_array().unwrap().len(), 2);

    let only =
        s.h.ok(
            "search3",
            &format!("query=&songCount=10&musicFolderId={}", s.family_id),
        )
        .await["searchResult3"]
            .clone();
    assert_eq!(only["song"].as_array().unwrap().len(), 1);
    assert_eq!(only["song"][0]["title"], "Two");

    let artists =
        s.h.ok("getArtists", &format!("musicFolderId={}", s.family_id))
            .await["artists"]["index"]
            .clone();
    assert_eq!(artists[0]["artist"][0]["name"], "Beta");
}

#[tokio::test(flavor = "multi_thread")]
async fn restricted_user_cannot_reach_the_other_library_anywhere() {
    let s = setup().await;
    s.h.ok(
        "createUser",
        "username=bob&password=bobpw&streamRole=true&downloadRole=true",
    )
    .await;
    let family_id = s.family_id;
    s.h.server
        .state
        .db
        .run(move |c| {
            let bob = users::by_name(c, "bob")?.unwrap();
            assert!(libraries::granted(c, &bob.id)?.contains(&family_id));
            libraries::set_access(c, &bob.id, &[libraries::DEFAULT_ID])?;
            Ok(())
        })
        .await
        .unwrap();

    let b = |endpoint: &'static str, query: String| {
        let h = &s.h;
        async move { as_user(h, "bob", "bobpw", endpoint, &query).await }
    };

    let folders = b("getMusicFolders", String::new()).await["musicFolders"]["musicFolder"].clone();
    assert_eq!(folders.as_array().unwrap().len(), 1);

    let found = b(
        "search3",
        "query=&songCount=10&albumCount=10&artistCount=10".into(),
    )
    .await["searchResult3"]
        .clone();
    assert_eq!(found["song"].as_array().unwrap().len(), 1);
    assert_eq!(found["album"].as_array().unwrap().len(), 1);
    assert_eq!(found["artist"].as_array().unwrap().len(), 1);
    let named = b("search3", "query=two".into()).await["searchResult3"].clone();
    assert!(named["song"].as_array().is_none_or(|v| v.is_empty()));

    let artists = b("getArtists", String::new()).await["artists"]["index"].clone();
    assert_eq!(artists.as_array().unwrap().len(), 1);

    let albums =
        b("getAlbumList2", "type=newest&size=10".into()).await["albumList2"]["album"].clone();
    assert_eq!(albums.as_array().unwrap().len(), 1);

    let random = b("getRandomSongs", "size=10".into()).await["randomSongs"]["song"].clone();
    assert_eq!(random.as_array().unwrap().len(), 1);

    let genres = b("getGenres", String::new()).await["genres"]["genre"].clone();
    assert_eq!(genres[0]["songCount"], 1);

    for (endpoint, id_key, id) in [
        ("getSong", "id", &s.family_song),
        ("getAlbum", "id", &s.family_album),
        ("getArtist", "id", &s.family_artist),
        ("getMusicDirectory", "id", &s.family_album),
        ("getLyricsBySongId", "id", &s.family_song),
        ("star", "id", &s.family_song),
        ("setRating", "id", &s.family_song),
        ("scrobble", "id", &s.family_song),
    ] {
        let extra = if endpoint == "setRating" {
            "&rating=3"
        } else {
            ""
        };
        let v = b(endpoint, format!("{id_key}={id}{extra}")).await;
        let resp = &v;
        let failed = resp["status"] == "failed"
            || resp["lyricsList"]["structuredLyrics"]
                .as_array()
                .is_some_and(|l| l.is_empty());
        assert!(failed, "{endpoint} leaked: {v}");
    }

    for endpoint in ["stream", "download", "getCoverArt"] {
        let id = if endpoint == "getCoverArt" {
            &s.family_album
        } else {
            &s.family_song
        };
        let r =
            s.h.get(&format!(
                "/rest/{endpoint}?u=bob&p=bobpw&f=json&c=test&id={id}"
            ))
            .await;
        let kind = r.headers[axum::http::header::CONTENT_TYPE]
            .to_str()
            .unwrap();
        assert!(
            !kind.starts_with("audio") && !kind.starts_with("image"),
            "{endpoint} served the file as {kind}"
        );
        assert_eq!(r.json()["status"], "failed", "{endpoint}");
    }
    let own =
        s.h.get(&format!(
            "/rest/stream?u=bob&p=bobpw&c=test&id={}",
            s.own_song
        ))
        .await;
    assert!(
        own.headers[axum::http::header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("audio")
    );

    let pl = b(
        "createPlaylist",
        format!("name=mix&songId={}", s.family_song),
    )
    .await;
    assert_eq!(pl["status"], "failed");
    let jb = b("jukeboxControl", format!("action=add&id={}", s.family_song)).await;
    assert_eq!(jb["status"], "failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_library_drops_its_songs_only() {
    let s = setup().await;
    let id = s.family_id;
    s.h.server
        .state
        .db
        .run(move |c| {
            libraries::delete(c, id)?;
            Ok(())
        })
        .await
        .unwrap();
    let all = s.h.ok("search3", "query=&songCount=10").await["searchResult3"].clone();
    assert_eq!(all["song"].as_array().unwrap().len(), 1);
    assert_eq!(all["song"][0]["title"], "One");
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_go_to_the_chosen_library_and_respect_read_only() {
    let s = setup().await;
    let bytes = tagged_wav_bytes(
        73,
        &Tags {
            title: Some("Fresh"),
            artist: Some("Gamma"),
            album: Some("Third"),
            ..Default::default()
        },
    );
    let q = format!("{}&musicFolderId={}", s.h.auth(), s.family_id);
    let v = s.h.upload(&q, "fresh.wav", &bytes).await;
    assert_eq!(v["status"], "ok", "{v}");
    let landed = s._family.path().join("Gamma");
    assert!(landed.is_dir(), "upload should land in the family library");
    assert!(!s.h.music.path().join("Gamma").exists());
    assert!(!s.h.music.path().join(".juuz-tmp").exists());

    let own = tagged_wav_bytes(
        74,
        &Tags {
            title: Some("Default"),
            artist: Some("Delta"),
            album: Some("Fourth"),
            ..Default::default()
        },
    );
    let v = s.h.upload(&s.h.auth(), "default.wav", &own).await;
    assert_eq!(v["status"], "ok", "{v}");
    assert!(s.h.music.path().join("Delta").is_dir());

    let id = s.family_id;
    s.h.server
        .state
        .db
        .run(move |c| {
            libraries::update(c, id, "Family", false, 0).unwrap();
            Ok(())
        })
        .await
        .unwrap();
    let again = tagged_wav_bytes(
        75,
        &Tags {
            title: Some("Nope"),
            artist: Some("Gamma"),
            album: Some("Third"),
            ..Default::default()
        },
    );
    let v = s.h.upload(&q, "nope.wav", &again).await;
    assert_eq!(v["status"], "failed", "{v}");
}
