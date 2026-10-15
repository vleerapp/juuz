mod common;

use std::path::Path;

use axum::body::Body;
use axum::http::{Request, header};
use common::*;
use serde_json::Value;

fn library_files(h: &Harness) -> Vec<String> {
    let root = h.root();
    walk(&root)
        .into_iter()
        .map(|p| {
            p.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|p| !p.starts_with(".juuz-tmp"))
        .collect()
}

fn tmp_leftovers(h: &Harness) -> usize {
    let dir = h.root().join(".juuz-tmp");
    if dir.exists() { count_files(&dir) } else { 0 }
}

fn result(v: &Value) -> &Value {
    assert_eq!(v["status"], "ok", "{v}");
    &v["juuzUploadResult"]
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_with_overrides_is_tagged_placed_and_indexed() {
    let h = Harness::new();
    let key = h.api_key().await;
    let wav = wav_bytes(1, 500);
    let v = h
        .upload(
            &format!("apiKey={key}&title=Hello&artist=Some%20Artist&album=Nice%20Album&year=2015&track=3&genre=Techno"),
            "whatever.wav",
            &wav,
        )
        .await;
    let r = result(&v);
    assert_eq!(r["status"], "created");
    assert_eq!(r["song"]["title"], "Hello");
    assert_eq!(r["song"]["artist"], "Some Artist");
    assert_eq!(r["song"]["album"], "Nice Album");
    assert_eq!(r["song"]["track"], 3);
    assert_eq!(r["song"]["year"], 2015);
    assert_eq!(r["song"]["genre"], "Techno");
    assert_eq!(
        r["song"]["path"],
        "Some Artist/2015 - Nice Album/03 - Hello.wav"
    );

    assert_eq!(
        library_files(&h),
        vec!["Some Artist/2015 - Nice Album/03 - Hello.wav"]
    );
    assert_eq!(tmp_leftovers(&h), 0);

    let id = r["song"]["id"].as_str().unwrap();
    let got = h.ok("getSong", &format!("id={id}")).await;
    assert_eq!(got["song"]["title"], "Hello");
    let found = h.ok("search3", "query=nice").await;
    assert_eq!(found["searchResult3"]["album"][0]["name"], "Nice Album");
    let artists = h.ok("getArtists", "").await;
    assert_eq!(
        artists["artists"]["index"][0]["artist"][0]["name"],
        "Some Artist"
    );

    let on_disk = lofty_title(
        &h.root()
            .join("Some Artist/2015 - Nice Album/03 - Hello.wav"),
    );
    assert_eq!(on_disk.as_deref(), Some("Hello"));
}

fn lofty_title(path: &Path) -> Option<String> {
    use lofty::file::TaggedFileExt;
    use lofty::tag::Accessor;
    let f = lofty::read_from_path(path).ok()?;
    let t = f.primary_tag().or_else(|| f.first_tag())?;
    t.title().map(|s| s.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn untagged_upload_uses_relative_path_hint() {
    let h = Harness::new();
    let key = h.api_key().await;
    let wav = wav_bytes(2, 500);
    let v = h
        .upload(
            &format!("apiKey={key}&relativePath=Soulseek%5CDaft%20Punk%5C2001%20-%20Discovery%5C03%20-%20Digital%20Love.wav"),
            "03 - Digital Love.wav",
            &wav,
        )
        .await;
    let r = result(&v);
    assert_eq!(r["status"], "created");
    assert_eq!(
        r["song"]["path"],
        "Daft Punk/2001 - Discovery/03 - Digital Love.wav"
    );
    assert_eq!(r["song"]["artist"], "Daft Punk");
    assert_eq!(r["song"]["album"], "Discovery");
    assert_eq!(r["song"]["title"], "Digital Love");
    assert_eq!(r["song"]["track"], 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn untagged_upload_without_hints_lands_in_unknown() {
    let h = Harness::new();
    let key = h.api_key().await;
    let v = h
        .upload(
            &format!("apiKey={key}"),
            "just a name.wav",
            &wav_bytes(3, 400),
        )
        .await;
    let r = result(&v);
    assert_eq!(r["song"]["title"], "just a name");
    assert_eq!(
        r["song"]["path"],
        "[Unknown Artist]/[Unknown Album]/just a name.wav"
    );
    assert_eq!(r["song"]["artist"], "[Unknown Artist]");
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_are_idempotent_by_audio_hash() {
    let h = Harness::new();
    let key = h.api_key().await;
    let q = format!("apiKey={key}&title=T&artist=A&album=B&track=1");
    let wav = wav_bytes(4, 500);
    let first = h.upload(&q, "x.wav", &wav).await;
    let second = h.upload(&q, "x.wav", &wav).await;
    assert_eq!(result(&first)["status"], "created");
    assert_eq!(result(&second)["status"], "duplicate");
    assert_eq!(result(&first)["song"]["id"], result(&second)["song"]["id"]);
    assert_eq!(library_files(&h).len(), 1);
    assert_eq!(tmp_leftovers(&h), 0);

    let other_album = h
        .upload(
            &format!("apiKey={key}&title=T&artist=A&album=Other&track=1"),
            "x.wav",
            &wav,
        )
        .await;
    assert_eq!(
        result(&other_album)["status"],
        "created",
        "same audio on another album is a new song"
    );
    assert_eq!(library_files(&h).len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_identical_uploads_create_one_song() {
    let h = std::sync::Arc::new(Harness::new());
    let key = h.api_key().await;
    let wav = std::sync::Arc::new(wav_bytes(5, 500));
    let mut tasks = Vec::new();
    for _ in 0..6 {
        let (h, wav, key) = (h.clone(), wav.clone(), key.clone());
        tasks.push(tokio::spawn(async move {
            h.upload(
                &format!("apiKey={key}&title=T&artist=A&album=B"),
                "x.wav",
                &wav,
            )
            .await
        }));
    }
    let mut created = 0;
    for t in tasks {
        let v = t.await.unwrap();
        if result(&v)["status"] == "created" {
            created += 1;
        }
    }
    assert_eq!(created, 1);
    assert_eq!(library_files(&h).len(), 1);
    assert_eq!(tmp_leftovers(&h), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_non_audio_and_empty_files() {
    let h = Harness::new();
    let key = h.api_key().await;
    let v = h
        .upload(
            &format!("apiKey={key}"),
            "a.flac",
            b"this is not audio at all",
        )
        .await;
    assert_eq!(v["status"], "failed");
    assert_eq!(v["error"]["code"], 0);
    let v = h.upload(&format!("apiKey={key}"), "a.flac", b"").await;
    assert_eq!(v["status"], "failed");
    assert!(library_files(&h).is_empty());
    assert_eq!(tmp_leftovers(&h), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn enforces_size_limit_and_cleans_up() {
    let h = Harness::build(None, |c| c.upload.max_size = 2000);
    let key = h.api_key().await;
    let v = h
        .upload(
            &format!("apiKey={key}&title=T"),
            "x.wav",
            &wav_bytes(6, 500),
        )
        .await;
    assert_eq!(v["status"], "failed");
    assert!(
        v["error"]["message"].as_str().unwrap().contains("limit"),
        "{v}"
    );
    assert!(library_files(&h).is_empty());
    assert_eq!(tmp_leftovers(&h), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn authentication_and_roles() {
    let h = Harness::new();
    let wav = wav_bytes(7, 400);

    let v = h.upload("title=T", "x.wav", &wav).await;
    assert_eq!(v["error"]["code"], 10);
    let v = h.upload("apiKey=jz_nope&title=T", "x.wav", &wav).await;
    assert_eq!(v["error"]["code"], 44);

    h.ok("createUser", "username=viewer&password=pw").await;
    let v = h.upload("u=viewer&p=pw&title=T", "x.wav", &wav).await;
    assert_eq!(v["error"]["code"], 50);

    h.ok("updateUser", "username=viewer&uploadRole=true").await;
    let v = h
        .upload("u=viewer&p=pw&title=Late&artist=A&album=B", "x.wav", &wav)
        .await;
    assert_eq!(result(&v)["status"], "created");

    let key = h.api_key().await;
    let v = h
        .upload_raw(
            "",
            &[
                ("apiKey", &key),
                ("title", "Form"),
                ("artist", "F"),
                ("album", "G"),
            ],
            "x.wav",
            &wav_bytes(8, 400),
        )
        .await
        .json();
    assert_eq!(
        result(&v)["status"],
        "created",
        "auth supplied as form fields before the file part"
    );

    let (ct, mut body) = multipart(&[], "x.wav", &wav);
    body.truncate(body.len() - "\r\n------juuztestboundary--\r\n".len());
    let tail =
        b"\r\n------juuztestboundary\r\nContent-Disposition: form-data; name=\"apiKey\"\r\n\r\n"
            .to_vec();
    body.extend(tail);
    body.extend(key.as_bytes());
    body.extend(b"\r\n------juuztestboundary--\r\n");
    let req = Request::builder()
        .method("POST")
        .uri("/rest/juuzUploadSong?f=json")
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body))
        .unwrap();
    let v = h.raw(req).await.json();
    assert_eq!(
        v["status"], "failed",
        "credentials after the file part must not authorize the upload: {v}"
    );
    assert_eq!(library_files(&h).len(), 2);
    assert_eq!(tmp_leftovers(&h), 0);

    let v = h
        .get(&format!("/rest/juuzUploadSong?f=json&apiKey={key}"))
        .await
        .json();
    assert_eq!(v["status"], "failed");

    let disabled = Harness::build(None, |c| c.upload.enabled = false);
    let key = disabled.api_key().await;
    let v = disabled
        .upload(&format!("apiKey={key}&title=T"), "x.wav", &wav)
        .await;
    assert_eq!(v["error"]["code"], 50);
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_names_never_escape_the_library() {
    let h = Harness::new();
    let key = h.api_key().await;
    let wav = wav_bytes(9, 400);
    let evil = "..%2F..%2F..%2Fetc%2Fpwn";
    let v = h
        .upload(
            &format!("apiKey={key}&title={evil}&artist={evil}&album={evil}&relativePath={evil}%2F..%2F..%2Fx.wav"),
            "../../../evil.wav",
            &wav,
        )
        .await;
    assert_eq!(result(&v)["status"], "created");
    let root = h.root();
    for f in walk(&root) {
        let canon = std::fs::canonicalize(&f).unwrap();
        assert!(canon.starts_with(&root), "{} escaped", canon.display());
        assert!(
            !f.strip_prefix(&root)
                .unwrap()
                .components()
                .any(|c| c.as_os_str() == "..")
        );
    }
    let parent = root.parent().unwrap();
    assert!(!parent.join("etc").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn cover_and_lyrics_companions() {
    let h = Harness::new();
    let key = h.api_key().await;
    let v = h
        .upload(
            &format!("apiKey={key}&title=T&artist=A&album=B&track=1"),
            "x.wav",
            &wav_bytes(10, 400),
        )
        .await;
    let song = result(&v)["song"].clone();
    let sid = song["id"].as_str().unwrap();
    let aid = song["albumId"].as_str().unwrap();

    let png = png_bytes();
    let v = h
        .upload(
            &format!("apiKey={key}&kind=cover&albumId={aid}"),
            "c.png",
            &png,
        )
        .await;
    assert_eq!(result(&v)["kind"], "cover");
    let art = h
        .get(&format!("/rest/getCoverArt?{}&id={aid}", h.auth()))
        .await;
    assert_eq!(art.headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(art.body, png);
    let small = h
        .get(&format!("/rest/getCoverArt?{}&id={sid}&size=16", h.auth()))
        .await;
    assert_eq!(small.headers[header::CONTENT_TYPE], "image/jpeg");
    let bad = h
        .upload(
            &format!("apiKey={key}&kind=cover&albumId={aid}"),
            "c.png",
            b"nope",
        )
        .await;
    assert_eq!(bad["status"], "failed");

    let v = h
        .upload(
            &format!("apiKey={key}&kind=lyrics&songId={sid}"),
            "l.lrc",
            b"[00:02.00]line one\n",
        )
        .await;
    assert_eq!(result(&v)["kind"], "lyrics");
    let l = h.ok("getLyricsBySongId", &format!("id={sid}")).await;
    assert_eq!(
        l["lyricsList"]["structuredLyrics"][0]["line"][0]["start"],
        2000
    );
    let files = library_files(&h);
    assert!(files.iter().any(|f| f.ends_with("cover.png")), "{files:?}");
    assert!(files.iter().any(|f| f.ends_with(".lrc")), "{files:?}");
    assert_eq!(
        h.call("juuzUploadSong", "kind=bogus").await["status"],
        "failed"
    );
}

fn png_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    let img = image::RgbImage::from_pixel(64, 64, image::Rgb([200, 40, 40]));
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_removes_file_rows_and_empty_dirs() {
    let h = Harness::new();
    let key = h.api_key().await;
    let v = h
        .upload(
            &format!("apiKey={key}&title=T&artist=Gone&album=Soon&track=1"),
            "x.wav",
            &wav_bytes(11, 400),
        )
        .await;
    let sid = result(&v)["song"]["id"].as_str().unwrap().to_string();
    let aid = result(&v)["song"]["albumId"].as_str().unwrap().to_string();
    h.upload(
        &format!("apiKey={key}&kind=cover&albumId={aid}"),
        "c.png",
        &png_bytes(),
    )
    .await;
    assert!(library_files(&h).len() >= 2);

    h.ok("createUser", "username=viewer&password=pw").await;
    let denied = h
        .get(&format!(
            "/rest/juuzDeleteSong?f=json&u=viewer&p=pw&id={sid}"
        ))
        .await
        .json();
    assert_eq!(denied["error"]["code"], 50);

    h.ok("juuzDeleteSong", &format!("id={sid}")).await;
    assert!(library_files(&h).is_empty(), "{:?}", library_files(&h));
    assert!(!h.root().join("Gone").exists());
    assert_eq!(
        h.call("getSong", &format!("id={sid}")).await["error"]["code"],
        70
    );
    assert!(
        h.ok("search3", "query=gone").await["searchResult3"]["artist"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        h.call("juuzDeleteSong", &format!("id={sid}")).await["error"]["code"],
        70
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn uploaded_files_join_existing_albums() {
    let h = Harness::new();
    make_track(
        h.music.path(),
        "Band/2010 - Record/01 - First.wav",
        40,
        &Tags {
            title: Some("First"),
            artist: Some("Band"),
            album: Some("Record"),
            track: Some(1),
            year: Some("2010"),
            ..Default::default()
        },
    );
    h.scan().await;
    let key = h.api_key().await;
    let v = h
        .upload(
            &format!("apiKey={key}&title=Second&artist=Band&album=Record&track=2&year=2010"),
            "x.wav",
            &wav_bytes(41, 400),
        )
        .await;
    let r = result(&v);
    assert_eq!(r["song"]["path"], "Band/2010 - Record/02 - Second.wav");
    let album = h
        .ok(
            "getAlbum",
            &format!("id={}", r["song"]["albumId"].as_str().unwrap()),
        )
        .await;
    assert_eq!(album["album"]["songCount"], 2);
    assert_eq!(
        h.ok("getAlbumList2", "type=newest").await["albumList2"]["album"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
