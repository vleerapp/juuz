mod common;

use axum::http::header;
use common::*;

fn as_user(user: &str, pass: &str, endpoint: &str, query: &str) -> String {
    format!("/rest/{endpoint}?u={user}&p={pass}&f=json&c=test&{query}")
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_bad_passwords_lock_only_the_offending_ip() {
    let h = Harness::new();
    h.ok("createUser", "username=bob&password=right").await;
    for _ in 0..10 {
        let v = h
            .get_from("203.0.113.5", &as_user("bob", "wrong", "ping", ""))
            .await
            .json();
        assert_eq!(v["error"]["code"], 40);
    }
    let locked = h
        .get_from("203.0.113.5", &as_user("bob", "right", "ping", ""))
        .await
        .json();
    assert_eq!(locked["error"]["code"], 40, "{locked}");
    let elsewhere = h
        .get_from("203.0.113.6", &as_user("bob", "right", "ping", ""))
        .await
        .json();
    assert_eq!(elsewhere["status"], "ok", "{elsewhere}");
    let admin = h
        .get_from("203.0.113.6", &as_user(USER, PASS, "ping", ""))
        .await
        .json();
    assert_eq!(admin["status"], "ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn request_floods_get_429_per_ip() {
    let h = Harness::build(None, |c| {
        c.ratelimit.per_minute = 60;
        c.ratelimit.burst = 5;
    });
    let uri = format!("/rest/ping?{}", h.auth());
    for _ in 0..5 {
        assert_eq!(h.get_from("198.51.100.1", &uri).await.status, 200);
    }
    let blocked = h.get_from("198.51.100.1", &uri).await;
    assert_eq!(blocked.status, 429);
    assert!(blocked.headers.contains_key(header::RETRY_AFTER));
    assert_eq!(blocked.json()["status"], "failed");
    assert_eq!(h.get_from("198.51.100.2", &uri).await.status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn forwarded_for_is_ignored_unless_the_proxy_is_trusted() {
    let uri = |h: &Harness| format!("/rest/ping?{}", h.auth());
    let h = Harness::build(None, |c| {
        c.ratelimit.per_minute = 60;
        c.ratelimit.burst = 2;
    });
    for n in 0..2 {
        let xff = format!("9.9.9.{n}");
        assert_eq!(
            h.get_via("10.0.0.1", &[("x-forwarded-for", &xff)], &uri(&h))
                .await
                .status,
            200
        );
    }
    assert_eq!(
        h.get_via("10.0.0.1", &[("x-forwarded-for", "9.9.9.9")], &uri(&h))
            .await
            .status,
        429
    );

    let h = Harness::build(None, |c| {
        c.ratelimit.per_minute = 60;
        c.ratelimit.burst = 2;
        c.ratelimit.trust_proxy = true;
    });
    for n in 0..4 {
        let xff = format!("1.1.1.1, 9.9.9.{n}");
        assert_eq!(
            h.get_via("10.0.0.1", &[("x-forwarded-for", &xff)], &uri(&h))
                .await
                .status,
            200
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn api_keys_cannot_change_passwords_or_mint_keys() {
    let h = Harness::new();
    let key = h.api_key().await;
    for (endpoint, query) in [
        ("changePassword", "username=admin&password=owned"),
        ("juuzCreateApiKey", "name=sneaky"),
        ("updateUser", "username=admin&password=owned"),
    ] {
        let v = h
            .get(&format!("/rest/{endpoint}?apiKey={key}&f=json&{query}"))
            .await
            .json();
        assert_eq!(v["error"]["code"], 50, "{endpoint}: {v}");
    }
    assert_eq!(
        h.get(&format!("/rest/ping?apiKey={key}&f=json"))
            .await
            .json()["status"],
        "ok"
    );
    assert_eq!(h.call("ping", "").await["status"], "ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn admins_cannot_delete_or_demote_themselves() {
    let h = Harness::new();
    let v = h.call("deleteUser", &format!("username={USER}")).await;
    assert_eq!(v["error"]["code"], 50, "{v}");
    let v = h
        .call("updateUser", &format!("username={USER}&adminRole=false"))
        .await;
    assert_eq!(v["error"]["code"], 50, "{v}");
    h.ok("createUser", "username=bob&password=pw").await;
    h.ok("deleteUser", "username=bob").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn private_playlists_of_others_cannot_be_shared() {
    let h = Harness::new();
    h.ok("createUser", "username=bob&password=pw&shareRole=true")
        .await;
    let made = h.ok("createPlaylist", "name=secret").await;
    let id = made["playlist"]["id"].as_str().unwrap().to_string();
    let v = h
        .get(&as_user("bob", "pw", "createShare", &format!("id={id}")))
        .await
        .json();
    assert_eq!(v["error"]["code"], 70, "{v}");
    let own = h.call("createShare", &format!("id={id}")).await;
    assert_eq!(own["status"], "ok", "{own}");
}

#[tokio::test(flavor = "multi_thread")]
async fn lyrics_for_titles_with_dots_land_next_to_the_song() {
    let h = Harness::new();
    let key = h.api_key().await;
    let v = h
        .upload(
            &format!(
                "apiKey={key}&title=Mr.%20Brightside&artist=The%20Killers&album=Hot%20Fuss&track=2"
            ),
            "x.wav",
            &wav_bytes(21, 400),
        )
        .await;
    let sid = v["juuzUploadResult"]["song"]["id"].as_str().unwrap();
    let up = h
        .upload(
            &format!("apiKey={key}&kind=lyrics&songId={sid}"),
            "l.lrc",
            b"[00:02.00]line one\n",
        )
        .await;
    assert_eq!(up["juuzUploadResult"]["kind"], "lyrics");
    let files = walk(&h.root());
    assert!(
        files
            .iter()
            .any(|f| f.file_name().unwrap() == "02 - Mr. Brightside.lrc"),
        "{files:?}"
    );
    let l = h.ok("getLyricsBySongId", &format!("id={sid}")).await;
    assert_eq!(
        l["lyricsList"]["structuredLyrics"][0]["line"][0]["start"],
        2000
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn podcast_media_never_gets_a_scriptable_content_type() {
    let feed = |base: &str| {
        format!(
            r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Evil</title>
<item><guid>g1</guid><title>Ep</title><enclosure url="{base}/ep.mp3" length="9" type="text/html"/></item>
</channel></rss>"#
        )
    };
    let mock = mock_server(|base| {
        vec![
            route("/feed.xml", 200, feed(base)),
            route("/ep.mp3", 200, "<script>alert(1)</script>"),
        ]
    });
    let h = Harness::build(None, |c| c.podcasts.allow_private_hosts = true);
    h.ok(
        "createPodcastChannel",
        &format!("url={}/feed.xml", mock.url),
    )
    .await;
    let mut ep = String::new();
    for _ in 0..100 {
        let v = h.ok("getPodcasts", "").await;
        if let Some(id) = v["podcasts"]["channel"][0]["episode"][0]["id"].as_str() {
            ep = id.to_string();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    h.ok("downloadPodcastEpisode", &format!("id={ep}")).await;
    for _ in 0..100 {
        let v = h.ok("getPodcasts", "").await;
        if v["podcasts"]["channel"][0]["episode"][0]["status"] == "completed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let r = h.get(&format!("/rest/stream?{}&id={ep}", h.auth())).await;
    let ct = r.headers[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.starts_with("audio/"), "{ct}");
    assert_eq!(r.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(r.headers[header::CONTENT_SECURITY_POLICY], "sandbox");
}
