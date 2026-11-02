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
async fn api_keys_cannot_mint_keys() {
    let h = Harness::new();
    let key = h.api_key().await;
    for (endpoint, query) in [("juuzCreateApiKey", "name=sneaky")] {
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
async fn token_auth_can_be_switched_off() {
    let h = Harness::build(None, |c| c.token_auth = false);
    let token = juuz::auth::token_for(PASS, "saltsalt");
    let v = h
        .get(&format!(
            "/rest/ping?u={USER}&t={token}&s=saltsalt&f=json&c=test"
        ))
        .await
        .json();
    assert_eq!(v["error"]["code"], 42, "{v}");
    assert_eq!(h.call("ping", "").await["status"], "ok");
    h.ok("createUser", "username=bob&password=pw").await;
    let v = h.get(&as_user("bob", "pw", "ping", "")).await.json();
    assert_eq!(v["status"], "ok", "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn removed_endpoints_are_unknown() {
    let h = Harness::new();
    for endpoint in [
        "getChatMessages",
        "addChatMessage",
        "getPodcasts",
        "createPodcastChannel",
        "createUser",
        "updateUser",
        "deleteUser",
        "changePassword",
        "getVideos",
        "getVideoInfo",
        "getCaptions",
        "hls",
        "getAvatar",
    ] {
        let v = h
            .get(&format!("/rest/{endpoint}?{}", h.auth()))
            .await
            .json();
        assert_eq!(v["status"], "failed", "{endpoint}: {v}");
        assert_eq!(v["error"]["message"], "unknown endpoint", "{endpoint}: {v}");
    }
}
