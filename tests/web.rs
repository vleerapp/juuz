mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::*;
use futures_util::StreamExt;

fn md5_token(password: &str, salt: &str) -> String {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(password.as_bytes());
    h.update(salt.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn location(r: &Raw) -> String {
    r.headers
        .get(header::LOCATION)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

fn cookie(r: &Raw) -> Option<String> {
    r.headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("juuz_session=") && !v.starts_with("juuz_session=;"))
        .map(|v| v.split(';').next().unwrap().to_string())
}

async fn post(h: &Harness, uri: &str, session: Option<&str>, body: &str) -> Raw {
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(c) = session {
        req = req.header(header::COOKIE, c);
    }
    h.raw(req.body(Body::from(body.to_string())).unwrap()).await
}

async fn page(h: &Harness, uri: &str, session: Option<&str>) -> Raw {
    let mut req = Request::builder().uri(uri);
    if let Some(c) = session {
        req = req.header(header::COOKIE, c);
    }
    h.raw(req.body(Body::empty()).unwrap()).await
}

async fn login(h: &Harness, name: &str, pass: &str) -> String {
    let r = post(
        h,
        "/login",
        None,
        &format!("username={name}&password={pass}"),
    )
    .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.text());
    assert_eq!(location(&r), "/");
    cookie(&r).expect("session cookie")
}

#[tokio::test(flavor = "multi_thread")]
async fn anonymous_gets_login_and_rest_stays_subsonic() {
    let h = Harness::new();
    let home = page(&h, "/", None).await;
    assert_eq!(home.status, StatusCode::OK);
    let html = home.text();
    assert!(html.contains("sign in"));
    assert!(!html.contains("api keys"));
    assert!(html.contains("juuz.css?v="));

    let css = page(&h, "/juuz.css", None).await;
    assert_eq!(css.status, StatusCode::OK);
    assert!(
        css.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/css")
    );

    let (first, again) = (css.text(), page(&h, "/juuz.css", None).await.text());
    assert!(first.contains("caret-shape"));
    assert_eq!(first, again);
    assert!(
        css.headers[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .starts_with("private")
    );

    let font = page(&h, "/departure-mono.woff2", None).await;
    assert_eq!(font.status, StatusCode::OK);
    assert_eq!(font.headers[header::CONTENT_TYPE], "font/woff2");
    assert!(font.headers.contains_key(header::CACHE_CONTROL));
    assert!(font.body.starts_with(b"wOF2"));

    let client = page(&h, "/client.js", None).await;
    assert_eq!(client.status, StatusCode::OK);
    assert!(
        client.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/javascript")
    );
    let script = client.text();
    assert!(script.contains("caret-shape"));
    assert!(script.contains("juuz.grid"));
    assert!(script.contains("new WebSocket(url)"));
    assert!(html.contains("src=\"/client.js?v="));

    let missing = page(&h, "/rest/doesNotExist?f=json", None).await;
    assert_eq!(missing.json()["status"], "failed");
    let ping = h.get(&format!("/rest/ping?{}", h.auth())).await;
    assert_eq!(ping.json()["status"], "ok");

    let post_anon = post(&h, "/account/password", None, "current=a&new=b&confirm=b").await;
    assert_eq!(post_anon.status, StatusCode::SEE_OTHER);
    assert_eq!(location(&post_anon), "/");
}

#[tokio::test(flavor = "multi_thread")]
async fn login_dashboard_logout() {
    let h = Harness::new();
    let bad = post(
        &h,
        "/login",
        None,
        &format!("username={USER}&password=nope"),
    )
    .await;
    assert_eq!(bad.status, StatusCode::SEE_OTHER);
    assert_eq!(location(&bad), "/?n=login-failed");
    assert!(cookie(&bad).is_none());

    let shown = page(&h, "/?n=login-failed", None).await.text();
    assert!(shown.contains("wrong username or password"));
    let spoof = page(&h, "/?n=anything%20else", None).await.text();
    assert!(!spoof.contains("anything else"));

    let session = login(&h, USER, PASS).await;
    let dash = page(&h, "/", Some(&session)).await.text();
    assert!(dash.contains("sign out"));
    assert!(dash.contains("admin"));
    assert!(!dash.contains("sign in"));
    let keys = page(&h, "/account/keys", Some(&session)).await.text();
    assert!(keys.contains("api keys"));

    let out = post(&h, "/logout", Some(&session), "").await;
    assert_eq!(out.status, StatusCode::SEE_OTHER);
    let after = page(&h, "/", Some(&session)).await.text();
    assert!(after.contains("sign in"));
    assert!(!after.contains("sign out"));
}

#[tokio::test(flavor = "multi_thread")]
async fn every_page_renders_html_not_subsonic_json() {
    let h = Harness::new();
    let session = login(&h, USER, PASS).await;
    for uri in ["/", "/account", "/account/keys", "/admin", "/admin/server"] {
        let r = page(&h, uri, Some(&session)).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}");
        assert!(
            r.headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/html"),
            "{uri}"
        );
        let html = r.text();
        assert!(html.contains("class=\"tabs\""), "{uri}");
        assert!(html.contains("sign out"), "{uri}");
    }
    for uri in ["/account", "/account/keys", "/admin", "/admin/server"] {
        let r = page(&h, uri, None).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{uri}");
        assert_eq!(location(&r), "/", "{uri}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn password_change_keeps_token_auth_and_drops_other_sessions() {
    let h = Harness::new();
    let first = login(&h, USER, PASS).await;
    let second = login(&h, USER, PASS).await;

    let wrong = post(
        &h,
        "/account/password",
        Some(&first),
        "current=bad&new=fresh&confirm=fresh",
    )
    .await;
    assert_eq!(location(&wrong), "/account?n=password-wrong");
    let mismatch = post(
        &h,
        "/account/password",
        Some(&first),
        &format!("current={PASS}&new=fresh&confirm=other"),
    )
    .await;
    assert_eq!(location(&mismatch), "/account?n=password-mismatch");

    let done = post(
        &h,
        "/account/password",
        Some(&first),
        &format!("current={PASS}&new=fresh&confirm=fresh"),
    )
    .await;
    assert_eq!(location(&done), "/account?n=password-changed");

    let salt = "abc123";
    let token = md5_token("fresh", salt);
    let ok = h
        .get(&format!("/rest/ping?u={USER}&t={token}&s={salt}&f=json"))
        .await;
    assert_eq!(ok.json()["status"], "ok");
    let stale = md5_token(PASS, salt);
    let old = h
        .get(&format!("/rest/ping?u={USER}&t={stale}&s={salt}&f=json"))
        .await;
    assert_eq!(old.json()["status"], "failed");

    assert!(
        page(&h, "/", Some(&first))
            .await
            .text()
            .contains("sign out")
    );
    assert!(
        page(&h, "/", Some(&second))
            .await
            .text()
            .contains("sign in")
    );
    login(&h, USER, "fresh").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_actions_are_gated() {
    let h = Harness::new();
    h.ok("createUser", "username=bob&password=bobpw").await;
    let bob = login(&h, "bob", "bobpw").await;

    let dash = page(&h, "/account/keys", Some(&bob)).await.text();
    assert!(dash.contains("api keys"));
    assert!(!dash.contains("new user"));
    assert!(!dash.contains("library scan"));
    assert!(!dash.contains("href=\"/admin\""));
    for uri in ["/admin", "/admin/server"] {
        let r = page(&h, uri, Some(&bob)).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{uri}");
        assert_eq!(location(&r), "/?n=forbidden", "{uri}");
    }

    for (uri, body) in [
        ("/admin/users/create", "username=eve&password=x"),
        ("/admin/users/update", "username=admin&admin=on"),
        ("/admin/users/delete", "username=admin"),
        ("/admin/scan", ""),
    ] {
        let r = post(&h, uri, Some(&bob), body).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{uri}");
        assert_eq!(location(&r), "/?n=forbidden", "{uri}");
    }
    let users = h.ok("getUsers", "").await;
    assert_eq!(users["users"]["user"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_manages_users_without_locking_self_out() {
    let h = Harness::new();
    let admin = login(&h, USER, PASS).await;

    let made = post(
        &h,
        "/admin/users/create",
        Some(&admin),
        "username=carol&password=carolpw",
    )
    .await;
    assert_eq!(location(&made), "/admin?n=user-created");
    let dup = post(
        &h,
        "/admin/users/create",
        Some(&admin),
        "username=Carol&password=x",
    )
    .await;
    assert_eq!(location(&dup), "/admin?n=user-exists");

    let carol = login(&h, "carol", "carolpw").await;
    let salt = "s4lt";
    let token = md5_token("carolpw", salt);
    let ping = h
        .get(&format!("/rest/ping?u=carol&t={token}&s={salt}&f=json"))
        .await;
    assert_eq!(ping.json()["status"], "ok");

    let reset = post(
        &h,
        "/admin/users/update",
        Some(&admin),
        "username=carol&password=newpw&stream=on&upload=on",
    )
    .await;
    assert_eq!(location(&reset), "/admin?n=user-updated");
    assert!(page(&h, "/", Some(&carol)).await.text().contains("sign in"));
    let token = md5_token("newpw", salt);
    let ping = h
        .get(&format!("/rest/ping?u=carol&t={token}&s={salt}&f=json"))
        .await;
    assert_eq!(ping.json()["status"], "ok");
    let info = h.ok("getUser", "username=carol").await;
    assert_eq!(info["user"]["uploadRole"], true);
    assert_eq!(info["user"]["adminRole"], false);

    let demote = post(&h, "/admin/users/update", Some(&admin), "username=admin").await;
    assert_eq!(location(&demote), "/admin?n=self-protected");
    let suicide = post(&h, "/admin/users/delete", Some(&admin), "username=admin").await;
    assert_eq!(location(&suicide), "/admin?n=self-protected");
    assert!(
        page(&h, "/", Some(&admin))
            .await
            .text()
            .contains("sign out")
    );

    let gone = post(&h, "/admin/users/delete", Some(&admin), "username=carol").await;
    assert_eq!(location(&gone), "/admin?n=user-deleted");
    let users = h.ok("getUsers", "").await;
    assert_eq!(users["users"]["user"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn profile_and_api_keys() {
    let h = Harness::new();
    let session = login(&h, USER, PASS).await;

    let saved = post(
        &h,
        "/account/profile",
        Some(&session),
        &format!("username={USER}&max_bit_rate=192"),
    )
    .await;
    assert_eq!(location(&saved), "/account?n=saved");
    let me = h.ok("getUser", "").await;
    assert_eq!(me["user"]["maxBitRate"], 192);
    assert_eq!(me["user"]["scrobblingEnabled"], false);
    let bad = post(
        &h,
        "/account/profile",
        Some(&session),
        &format!("username={USER}&max_bit_rate=fast"),
    )
    .await;
    assert_eq!(location(&bad), "/account?n=invalid");

    let created = post(&h, "/account/apikeys", Some(&session), "name=laptop").await;
    assert_eq!(created.status, StatusCode::OK);
    let html = created.text();
    let key = html
        .split("class=\"secret\">")
        .nth(1)
        .and_then(|rest| rest.split('<').next())
        .expect("key shown")
        .to_string();
    let via_key = h.get(&format!("/rest/ping?apiKey={key}&f=json")).await;
    assert_eq!(via_key.json()["status"], "ok");

    let listed = page(&h, "/account/keys", Some(&session)).await.text();
    assert!(listed.contains("laptop"));
    assert!(!listed.contains(&key));

    let keys = h.ok("juuzGetApiKeys", "").await;
    let id = keys["juuzApiKeys"]["apiKey"][0]["id"].as_str().unwrap();
    let removed = post(
        &h,
        "/account/apikeys/delete",
        Some(&session),
        &format!("id={id}"),
    )
    .await;
    assert_eq!(location(&removed), "/account/keys?n=key-deleted");
    let dead = h.get(&format!("/rest/ping?apiKey={key}&f=json")).await;
    assert_eq!(dead.json()["status"], "failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn stats_reflect_plays() {
    let h = Harness::new();
    let tags = Tags {
        title: Some("Uniquetitle"),
        artist: Some("Stat Band"),
        album: Some("Stat Album"),
        track: Some(1),
        ..Default::default()
    };
    make_track(h.music.path(), "Stat Band/Stat Album/01.wav", 7, &tags);
    assert_eq!(h.scan().await, 1);
    let found = h.ok("search3", "query=Uniquetitle").await;
    let id = found["searchResult3"]["song"][0]["id"].as_str().unwrap();
    h.ok("scrobble", &format!("id={id}")).await;
    h.ok("scrobble", &format!("id={id}&time=1000000000000"))
        .await;

    let session = login(&h, USER, PASS).await;
    let dash = page(&h, "/?range=all", Some(&session)).await.text();
    assert!(dash.contains("Uniquetitle"));
    assert!(dash.contains("Stat Band"));
    assert!(dash.contains("Stat Album"));
    assert!(dash.contains("<span class=\"count\">2</span>"));

    h.ok("createUser", "username=dave&password=davepw").await;
    let dave = login(&h, "dave", "davepw").await;
    let mine = page(&h, "/?range=all&scope=all", Some(&dave)).await.text();
    assert!(!mine.contains("Uniquetitle"));
    let everyone = page(&h, "/?range=all&scope=all", Some(&session))
        .await
        .text();
    assert!(everyone.contains("Uniquetitle"));
}

async fn post_from(h: &Harness, origin: &str, site: &str, body: &str) -> Raw {
    let req = Request::builder()
        .method("POST")
        .uri("/login")
        .header(header::HOST, "juuz.local")
        .header(header::ORIGIN, origin)
        .header("sec-fetch-site", site)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body.to_string()))
        .unwrap();
    h.raw(req).await
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_origin_checks() {
    let h = Harness::new();
    let body = format!("username={USER}&password={PASS}");

    let same = post_from(&h, "http://juuz.local", "same-origin", &body).await;
    assert_eq!(same.status, StatusCode::SEE_OTHER);
    assert!(cookie(&same).is_some());

    let cross = post_from(&h, "http://evil.test", "cross-site", &body).await;
    assert_eq!(cross.status, StatusCode::FORBIDDEN);
    assert!(cookie(&cross).is_none());

    let req = Request::builder()
        .method("POST")
        .uri("/login")
        .header(header::HOST, "juuz.local")
        .header(header::ORIGIN, "http://evil.test")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    assert_eq!(h.raw(req).await.status, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn subsonic_password_change_revokes_web_sessions() {
    let h = Harness::new();
    h.ok("createUser", "username=erin&password=erinpw").await;
    let session = login(&h, "erin", "erinpw").await;
    assert!(
        page(&h, "/", Some(&session))
            .await
            .text()
            .contains("sign out")
    );

    h.ok("changePassword", "username=erin&password=newerin")
        .await;
    assert!(
        page(&h, "/", Some(&session))
            .await
            .text()
            .contains("sign in")
    );

    let session = login(&h, "erin", "newerin").await;
    h.ok("updateUser", "username=erin&password=third").await;
    assert!(
        page(&h, "/", Some(&session))
            .await
            .text()
            .contains("sign in")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn lastfm_link_uses_callback_with_state() {
    let h = Harness::build(None, |c| {
        c.lastfm.api_key = Some("key".into());
        c.lastfm.secret = Some("secret".into());
    });
    let session = login(&h, USER, PASS).await;

    let dash = page(&h, "/account", Some(&session)).await.text();
    assert!(dash.contains("/account/lastfm/start"));

    let req = Request::builder()
        .method("POST")
        .uri("/account/lastfm/start")
        .header(header::HOST, "juuz.local")
        .header(header::COOKIE, &session)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::empty())
        .unwrap();
    let started = h.raw(req).await;
    assert_eq!(started.status, StatusCode::SEE_OTHER);
    let to = location(&started);
    assert!(to.starts_with(
        "https://www.last.fm/api/auth/?api_key=key&cb=http%3A%2F%2Fjuuz.local%2Faccount%2Flastfm%2Fcallback%3Fstate%3D"
    ));
    let state = to.rsplit("%3D").next().unwrap().to_string();
    let jar = started
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("juuz_lastfm="))
        .expect("state cookie")
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert_eq!(jar, format!("juuz_lastfm={state}"));

    let cookies = format!("{session}; {jar}");
    let forged = page(
        &h,
        "/account/lastfm/callback?token=abc&state=0000",
        Some(&cookies),
    )
    .await;
    assert_eq!(location(&forged), "/account?n=lastfm-failed");
    let stateless = page(
        &h,
        &format!("/account/lastfm/callback?token=abc&state={state}"),
        Some(&session),
    )
    .await;
    assert_eq!(location(&stateless), "/account?n=lastfm-failed");
    let anon = page(&h, "/account/lastfm/callback?token=abc&state=x", None).await;
    assert_eq!(location(&anon), "/");
}

#[tokio::test(flavor = "multi_thread")]
async fn live_regions_and_events_need_a_session() {
    let h = Harness::new();
    let session = login(&h, USER, PASS).await;
    let dash_response = page(&h, "/", Some(&session)).await;
    assert_eq!(dash_response.headers[header::CACHE_CONTROL], "no-store");
    let dash = dash_response.text();
    assert!(dash.contains("data-live=\"stats\""));
    let server = page(&h, "/admin/server", Some(&session)).await.text();
    assert!(server.contains("data-live=\"scan\""));
    assert!(server.contains("jukebox output"));
    assert!(server.contains("none selected"));
}

async fn connect(
    addr: std::net::SocketAddr,
    session: Option<&str>,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Error,
> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://{addr}/events").into_client_request().unwrap();
    if let Some(cookie) = session {
        req.headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
    }
    tokio_tungstenite::connect_async(req).await.map(|(s, _)| s)
}

#[tokio::test(flavor = "multi_thread")]
async fn websocket_pushes_instantly_when_a_play_is_recorded() {
    let h = Harness::new();
    let tags = Tags {
        title: Some("Pulse Song"),
        artist: Some("Pulse Band"),
        album: Some("Pulse Album"),
        track: Some(1),
        ..Default::default()
    };
    make_track(h.music.path(), "Pulse Band/Pulse Album/01.wav", 9, &tags);
    assert_eq!(h.scan().await, 1);
    let found = h.ok("search3", "query=Pulse").await;
    let id = found["searchResult3"]["song"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let session = login(&h, USER, PASS).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = h.server.router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    assert!(connect(addr, None).await.is_err());

    let mut socket = connect(addr, Some(&session)).await.expect("handshake");
    let started = std::time::Instant::now();
    h.ok("scrobble", &format!("id={id}")).await;
    let message = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
        .await
        .expect("push in time")
        .expect("open")
        .expect("frame");
    assert_eq!(message.into_text().unwrap().as_str(), "pulse");
    assert!(started.elapsed() < std::time::Duration::from_millis(1500));

    h.ok("scrobble", &format!("id={id}&submission=false")).await;
    let again = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
        .await
        .expect("second push")
        .expect("open")
        .expect("frame");
    assert_eq!(again.into_text().unwrap().as_str(), "pulse");
}

#[tokio::test(flavor = "multi_thread")]
async fn chart_days_follow_the_browser_timezone() {
    let h = Harness::new();
    let session = login(&h, USER, PASS).await;
    let last_label = async |offset: &str| {
        let cookies = format!("{session}; juuz_tz={offset}");
        let html = page(&h, "/", Some(&cookies)).await.text();
        let from = html.find("class=\"chart\"").unwrap();
        let chart = &html[from..];
        let at = chart.rfind("class=\"date\">").unwrap();
        chart[at + 9..].split('<').next().unwrap().to_string()
    };
    let east = last_label("840").await;
    let west = last_label("-840").await;
    let utc = last_label("0").await;
    assert_ne!(east, west);
    assert_eq!(utc, last_label("abc").await);
    assert_eq!(utc, last_label("99999").await);
}

#[tokio::test(flavor = "multi_thread")]
async fn username_can_be_changed_from_the_profile() {
    let h = Harness::new();
    h.ok("createUser", "username=bob&password=bobpw").await;
    let session = login(&h, USER, PASS).await;

    let taken = post(
        &h,
        "/account/profile",
        Some(&session),
        "username=Bob&max_bit_rate=0",
    )
    .await;
    assert_eq!(location(&taken), "/account?n=user-exists");
    let empty = post(
        &h,
        "/account/profile",
        Some(&session),
        "username=%20&max_bit_rate=0",
    )
    .await;
    assert_eq!(location(&empty), "/account?n=invalid");

    let renamed = post(
        &h,
        "/account/profile",
        Some(&session),
        "username=boss&max_bit_rate=0&scrobbling=on",
    )
    .await;
    assert_eq!(location(&renamed), "/account?n=saved");
    let dash = page(&h, "/account", Some(&session)).await.text();
    assert!(dash.contains("value=\"boss\""));
    assert!(dash.contains("boss"));

    let token = md5_token(PASS, "xyz");
    let by_token = h
        .get(&format!("/rest/ping?u=boss&t={token}&s=xyz&f=json"))
        .await;
    assert_eq!(by_token.json()["status"], "ok");
    let old = h.get(&format!("/rest/ping?u={USER}&p={PASS}&f=json")).await;
    assert_eq!(old.json()["status"], "failed");
    login(&h, "boss", PASS).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cookies_are_secure_behind_https() {
    let h = Harness::new();
    let body = format!("username={USER}&password={PASS}");
    let attrs = |r: &Raw| {
        r.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with("juuz_session="))
            .unwrap()
            .to_ascii_lowercase()
    };

    let plain = post(&h, "/login", None, &body).await;
    assert!(!attrs(&plain).contains("secure"));
    assert!(attrs(&plain).contains("httponly"));

    let req = Request::builder()
        .method("POST")
        .uri("/login")
        .header("x-forwarded-proto", "https")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap();
    let proxied = h.raw(req).await;
    assert!(attrs(&proxied).contains("secure"));
}

#[tokio::test(flavor = "multi_thread")]
async fn logo_and_favicon_are_served() {
    let h = Harness::new();
    let logo = page(&h, "/logo.svg", None).await;
    assert_eq!(logo.status, 200);
    assert_eq!(logo.headers[header::CONTENT_TYPE], "image/svg+xml");
    assert!(logo.text().starts_with("<svg"));

    let icon = page(&h, "/favicon.ico", None).await;
    assert_eq!(icon.status, 200);
    assert_eq!(icon.headers[header::CONTENT_TYPE], "image/x-icon");
    assert_eq!(&icon.body[..4], &[0, 0, 1, 0]);

    let login = page(&h, "/", None).await.text();
    assert!(login.contains("rel=\"icon\""));
    assert!(login.contains("<img src=\"/logo.svg?v="));
    assert!(!login.contains("<h1>juuz</h1>"));
}
