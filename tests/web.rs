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
    let keys = page(&h, "/account", Some(&session)).await.text();
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
    for uri in ["/", "/library", "/account", "/users", "/server"] {
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
    for uri in ["/account", "/users", "/server"] {
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

    let dash = page(&h, "/account", Some(&bob)).await.text();
    assert!(dash.contains("api keys"));
    assert!(!dash.contains("new user"));
    assert!(!dash.contains("library scan"));
    assert!(!dash.contains("href=\"/users\""));
    for uri in ["/users", "/server"] {
        let r = page(&h, uri, Some(&bob)).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{uri}");
        assert_eq!(location(&r), "/?n=forbidden", "{uri}");
    }

    for (uri, body) in [
        ("/users/create", "username=eve&password=x"),
        ("/users/update", "username=admin&admin=on"),
        ("/users/delete", "username=admin"),
        ("/server/scan", ""),
    ] {
        let r = post(&h, uri, Some(&bob), body).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{uri}");
        assert_eq!(location(&r), "/?n=forbidden", "{uri}");
    }
    let users = h.ok("getUsers", "").await;
    assert_eq!(users["users"]["user"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn granted_roles_open_matching_pages_only() {
    let h = Harness::new();
    h.ok(
        "createUser",
        "username=sam&password=sampw&juuzScanRole=true",
    )
    .await;
    let sam = login(&h, "sam", "sampw").await;

    let server = page(&h, "/server", Some(&sam)).await;
    assert_eq!(server.status, StatusCode::OK);
    let html = server.text();
    assert!(html.contains("library scan"));
    assert!(!html.contains("jukebox output"));
    assert!(
        !page(&h, "/", Some(&sam))
            .await
            .text()
            .contains("href=\"/users\"")
    );

    let scan = post(&h, "/server/scan", Some(&sam), "").await;
    assert_ne!(location(&scan), "/?n=forbidden");
    let select = post(&h, "/server/jukebox/select", Some(&sam), "id=x").await;
    assert_eq!(location(&select), "/?n=forbidden");
    let users = page(&h, "/users", Some(&sam)).await;
    assert_eq!(location(&users), "/?n=forbidden");
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_manages_users_without_locking_self_out() {
    let h = Harness::new();
    let admin = login(&h, USER, PASS).await;

    let made = post(
        &h,
        "/users/create",
        Some(&admin),
        "username=carol&password=carolpw",
    )
    .await;
    assert_eq!(location(&made), "/users?n=user-created");
    let dup = post(
        &h,
        "/users/create",
        Some(&admin),
        "username=Carol&password=x",
    )
    .await;
    assert_eq!(location(&dup), "/users?n=user-exists");

    let carol = login(&h, "carol", "carolpw").await;
    let salt = "s4lt";
    let token = md5_token("carolpw", salt);
    let ping = h
        .get(&format!("/rest/ping?u=carol&t={token}&s={salt}&f=json"))
        .await;
    assert_eq!(ping.json()["status"], "ok");

    let reset = post(
        &h,
        "/users/update",
        Some(&admin),
        "username=carol&password=newpw&stream=on&upload=on",
    )
    .await;
    assert_eq!(location(&reset), "/users?n=user-updated");
    assert!(page(&h, "/", Some(&carol)).await.text().contains("sign in"));
    let token = md5_token("newpw", salt);
    let ping = h
        .get(&format!("/rest/ping?u=carol&t={token}&s={salt}&f=json"))
        .await;
    assert_eq!(ping.json()["status"], "ok");
    let info = h.ok("getUser", "username=carol").await;
    assert_eq!(info["user"]["uploadRole"], true);
    assert_eq!(info["user"]["adminRole"], false);

    let demote = post(&h, "/users/update", Some(&admin), "username=admin").await;
    assert_eq!(location(&demote), "/users?n=self-protected");
    let suicide = post(&h, "/users/delete", Some(&admin), "username=admin").await;
    assert_eq!(location(&suicide), "/users?n=self-protected");
    assert!(
        page(&h, "/", Some(&admin))
            .await
            .text()
            .contains("sign out")
    );

    let gone = post(&h, "/users/delete", Some(&admin), "username=carol").await;
    assert_eq!(location(&gone), "/users?n=user-deleted");
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

    let listed = page(&h, "/account", Some(&session)).await.text();
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
    assert_eq!(location(&removed), "/account?n=key-deleted");
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
    let server = page(&h, "/server", Some(&session)).await.text();
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
        let end = html[from..].find("</section>").unwrap();
        let chart = &html[from..from + end];
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

#[tokio::test(flavor = "multi_thread")]
async fn libraries_page_is_permissioned_and_manages_libraries() {
    let h = Harness::new();
    h.ok(
        "createUser",
        "username=lee&password=leepw&juuzLibraryRole=true",
    )
    .await;
    h.ok("createUser", "username=bob&password=bobpw").await;
    let lee = login(&h, "lee", "leepw").await;
    let bob = login(&h, "bob", "bobpw").await;
    let admin = login(&h, USER, PASS).await;

    let denied = page(&h, "/server", Some(&bob)).await;
    assert_eq!(location(&denied), "/?n=forbidden");
    let shown = page(&h, "/server", Some(&lee)).await;
    assert_eq!(shown.status, StatusCode::OK);
    let html = shown.text();
    assert!(html.contains("add library"));
    assert!(!html.contains("remove library"));

    let extra = tempfile::tempdir().unwrap();
    let body = format!("name=Family&path={}&writable=on", extra.path().display());
    let made = post(&h, "/server/libraries/create", Some(&lee), &body).await;
    assert_eq!(location(&made), "/server?n=library-added");
    let again = post(&h, "/server/libraries/create", Some(&lee), &body).await;
    assert_eq!(location(&again), "/server?n=library-exists");
    let nested = format!("name=Nested&path={}", h.music.path().display());
    let nested = post(&h, "/server/libraries/create", Some(&lee), &nested).await;
    assert_eq!(location(&nested), "/server?n=library-overlap");
    let missing = post(
        &h,
        "/server/libraries/create",
        Some(&lee),
        "name=Gone&path=/nope/nothing",
    )
    .await;
    assert_eq!(location(&missing), "/server?n=library-path");

    let folders = h.ok("getMusicFolders", "").await["musicFolders"]["musicFolder"].clone();
    assert_eq!(folders.as_array().unwrap().len(), 2);
    let id = folders[1]["id"].as_i64().unwrap();

    let no_delete = post(
        &h,
        "/server/libraries/delete",
        Some(&lee),
        &format!("id={id}"),
    )
    .await;
    assert_eq!(location(&no_delete), "/?n=forbidden");
    let removed = post(
        &h,
        "/server/libraries/delete",
        Some(&admin),
        &format!("id={id}"),
    )
    .await;
    assert_eq!(location(&removed), "/server?n=library-removed");
    let default_stays = post(&h, "/server/libraries/delete", Some(&admin), "id=1").await;
    assert_eq!(location(&default_stays), "/server?n=invalid");
}

#[tokio::test(flavor = "multi_thread")]
async fn player_streams_with_the_session_cookie_only() {
    let h = Harness::new();
    make_track(
        h.music.path(),
        "Alpha/First/01 - One.wav",
        81,
        &Tags {
            title: Some("One"),
            artist: Some("Alpha"),
            album: Some("First"),
            track: Some(1),
            ..Default::default()
        },
    );
    h.scan().await;
    let found = h.ok("search3", "query=one").await["searchResult3"].clone();
    let song = found["song"][0]["id"].as_str().unwrap().to_string();
    let album = found["song"][0]["albumId"].as_str().unwrap().to_string();
    h.ok(
        "createUser",
        "username=mute&password=mutepw&streamRole=false",
    )
    .await;

    let anon = page(&h, &format!("/play?id={song}"), None).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    let anon_art = page(&h, &format!("/art?id={album}"), None).await;
    assert_eq!(anon_art.status, StatusCode::UNAUTHORIZED);

    let admin = login(&h, USER, PASS).await;
    let listing = page(&h, "/library?view=songs", Some(&admin)).await;
    assert_eq!(listing.status, StatusCode::OK);
    let html = listing.text();
    assert!(html.contains(&format!("data-play=\"{song}\"")), "{html}");
    assert!(html.contains("href=\"/library\""));
    let played = page(&h, &format!("/play?id={song}"), Some(&admin)).await;
    assert_eq!(played.status, StatusCode::OK);
    assert!(
        played.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("audio")
    );
    let missing = page(&h, "/play?id=nope", Some(&admin)).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let script = page(&h, "/player.js", None).await;
    assert_eq!(script.status, StatusCode::OK);

    let mute = login(&h, "mute", "mutepw").await;
    let blocked = page(&h, &format!("/play?id={song}"), Some(&mute)).await;
    assert_eq!(blocked.status, StatusCode::FORBIDDEN);
    let denied = page(&h, "/library?view=songs", Some(&mute)).await;
    assert_eq!(location(&denied), "/?n=forbidden");
}

#[tokio::test(flavor = "multi_thread")]
async fn catalogue_lists_songs_and_lets_a_match_be_chosen() {
    let inner = &OMM_SONG[r#"{"data":"#.len()..OMM_SONG.len() - 1];
    let mock = mock_omm(vec![
        route("/search/song", 200, format!(r#"{{"data":[{inner}]}}"#)),
        route("/catalog/omm:song:1234567890", 200, OMM_SONG),
    ]);
    let h = Harness::with_omm(Some(mock.url.clone()));
    make_track(
        h.music.path(),
        "Daft Punk/Discovery/03 - Digital Love.wav",
        91,
        &Tags {
            title: Some("Digital Love"),
            artist: Some("Daft Punk"),
            album: Some("Discovery"),
            track: Some(3),
            ..Default::default()
        },
    );
    h.scan().await;
    let found = h.ok("search3", "query=digital").await["searchResult3"].clone();
    let song = found["song"][0]["id"].as_str().unwrap().to_string();
    h.ok("createUser", "username=bob&password=bobpw").await;
    h.ok(
        "createUser",
        "username=cat&password=catpw&juuzCatalogueRole=true",
    )
    .await;
    let bob = login(&h, "bob", "bobpw").await;
    let cat = login(&h, "cat", "catpw").await;

    let plain = page(&h, "/library?view=songs", Some(&bob)).await.text();
    assert!(plain.contains("Digital Love"));
    assert!(!plain.contains("choose"));
    assert!(!plain.contains("fetch metadata"));
    let blocked = page(&h, &format!("/library/candidates?id={song}"), Some(&bob)).await;
    assert_eq!(location(&blocked), "/?n=forbidden");
    let listing = page(&h, "/library?view=songs", Some(&cat)).await;
    assert_eq!(listing.status, StatusCode::OK);
    let html = listing.text();
    assert!(html.contains("Digital Love"));
    assert!(html.contains("1 songs, 0 with metadata"), "{html}");
    assert!(html.contains("data-swap=\"results\""));
    assert!(html.contains("data-search"));

    let choices = page(&h, &format!("/library/candidates?id={song}"), Some(&cat)).await;
    assert_eq!(choices.status, StatusCode::OK);
    let html = choices.text();
    assert!(html.contains("omm:song:1234567890"), "{html}");

    let bad = post(
        &h,
        "/library/pick",
        Some(&cat),
        &format!("id={song}&omm=not-an-id"),
    )
    .await;
    assert_eq!(location(&bad), "/library?n=invalid");
    let picked = post(
        &h,
        "/library/pick",
        Some(&cat),
        &format!("id={song}&omm=omm:song:1234567890"),
    )
    .await;
    assert_eq!(location(&picked), "/library?n=catalogue-matched");
    let matched = page(&h, "/library?view=songs&status=matched", Some(&cat))
        .await
        .text();
    assert!(matched.contains("omm:song:1234567890"));
    let song_json = h.ok("getSong", &format!("id={song}")).await;
    assert_eq!(song_json["song"]["ommId"], "omm:song:1234567890");

    let cleared = post(&h, "/library/clear", Some(&cat), &format!("id={song}")).await;
    assert_eq!(location(&cleared), "/library?n=catalogue-cleared");
    let after = page(&h, "/library?view=songs&status=matched", Some(&cat))
        .await
        .text();
    assert!(!after.contains("omm:song:1234567890"));
    let denied = post(
        &h,
        "/library/pick",
        Some(&bob),
        &format!("id={song}&omm=omm:song:1234567890"),
    )
    .await;
    assert_eq!(location(&denied), "/?n=forbidden");
}

#[tokio::test(flavor = "multi_thread")]
async fn choosing_another_match_replaces_what_the_wrong_one_filled() {
    let wrong_id = "omm:song:1234567890";
    let right_id = "omm:song:5555555555";
    let right = OMM_SONG
        .replace(wrong_id, right_id)
        .replace("2001-03-12", "1999-05-01")
        .replace("FRZ390100010", "USRC19900001")
        .replace("Electronic", "Rock");
    let mock = mock_omm(vec![
        route("/catalog/omm:song:1234567890", 200, OMM_SONG),
        route("/catalog/omm:song:5555555555", 200, right),
    ]);
    let h = Harness::with_omm(Some(mock.url.clone()));
    make_track(
        h.music.path(),
        "Daft Punk/Discovery/03 - Digital Love.wav",
        92,
        &Tags {
            title: Some("Digital Love"),
            artist: Some("Daft Punk"),
            album: Some("Discovery"),
            track: Some(3),
            ..Default::default()
        },
    );
    h.scan().await;
    let found = h.ok("search3", "query=digital").await["searchResult3"].clone();
    let song = found["song"][0]["id"].as_str().unwrap().to_string();
    let admin = login(&h, USER, PASS).await;

    for (omm, year, genre) in [(wrong_id, 2001, "Electronic"), (right_id, 1999, "Rock")] {
        let r = post(
            &h,
            "/library/pick",
            Some(&admin),
            &format!("id={song}&omm={omm}"),
        )
        .await;
        assert_eq!(location(&r), "/library?n=catalogue-matched");
        let v = h.ok("getSong", &format!("id={song}")).await["song"].clone();
        assert_eq!(v["ommId"], omm);
        assert_eq!(v["year"], year, "{v}");
        assert_eq!(v["genre"], genre, "{v}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_restricts_a_user_to_chosen_libraries() {
    let h = Harness::new();
    let extra = tempfile::tempdir().unwrap();
    let admin = login(&h, USER, PASS).await;
    let body = format!("name=Family&path={}&writable=on", extra.path().display());
    let made = post(&h, "/server/libraries/create", Some(&admin), &body).await;
    assert_eq!(location(&made), "/server?n=library-added");
    h.ok("createUser", "username=bob&password=bobpw").await;
    let bob = login(&h, "bob", "bobpw").await;

    let folders = |user: &'static str, pass: &'static str| {
        let h = &h;
        async move {
            h.get(&format!(
                "/rest/getMusicFolders?u={user}&p={pass}&f=json&c=t"
            ))
            .await
            .json()["musicFolders"]["musicFolder"]
                .as_array()
                .unwrap()
                .len()
        }
    };
    assert_eq!(folders("bob", "bobpw").await, 2);

    let denied = post(&h, "/users/libraries", Some(&bob), "username=bob&lib_1=on").await;
    assert_eq!(location(&denied), "/?n=forbidden");
    assert_eq!(folders("bob", "bobpw").await, 2);

    let saved = post(
        &h,
        "/users/libraries",
        Some(&admin),
        "username=bob&lib_1=on&upload=1",
    )
    .await;
    assert_eq!(location(&saved), "/users?n=access-saved");
    assert_eq!(folders("bob", "bobpw").await, 1);
    let bob_row = h
        .server
        .state
        .db
        .run(|c| Ok(juuz::db::users::by_name(c, "bob")?.unwrap()))
        .await
        .unwrap();
    assert_eq!(bob_row.upload_library_id, Some(1));

    let family = h.ok("getMusicFolders", "").await["musicFolders"]["musicFolder"][1]["id"]
        .as_i64()
        .unwrap();
    let admin_form = post(
        &h,
        "/users/libraries",
        Some(&admin),
        &format!("username={USER}&lib_1=on&upload={family}"),
    )
    .await;
    assert_eq!(location(&admin_form), "/users?n=access-saved");
    assert_eq!(folders(USER, PASS).await, 2);
    let admin_row = h
        .server
        .state
        .db
        .run(|c| Ok(juuz::db::users::by_name(c, USER)?.unwrap()))
        .await
        .unwrap();
    assert_eq!(admin_row.upload_library_id, Some(family));
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_section_follows_the_keys_permission() {
    let h = Harness::new();
    h.ok(
        "createUser",
        "username=kim&password=kimpw&juuzKeysRole=false",
    )
    .await;
    h.ok("createUser", "username=lou&password=loupw").await;
    let kim = login(&h, "kim", "kimpw").await;
    let lou = login(&h, "lou", "loupw").await;

    let hidden = page(&h, "/account", Some(&kim)).await.text();
    assert!(!hidden.contains("api keys"));
    assert!(!hidden.contains("create key"));
    let shown = page(&h, "/account", Some(&lou)).await.text();
    assert!(shown.contains("api keys"));
    assert!(shown.contains("create key"));

    let blocked = post(&h, "/account/apikeys", Some(&kim), "name=nope").await;
    assert_eq!(location(&blocked), "/?n=forbidden");
    let made = post(&h, "/account/apikeys", Some(&lou), "name=laptop").await;
    assert_eq!(made.status, StatusCode::OK);
    assert!(made.text().contains("shown once"));

    let denied = h
        .get("/rest/juuzCreateApiKey?u=kim&p=kimpw&f=json&c=t&name=x")
        .await
        .json();
    assert_eq!(denied["status"], "failed");
    let allowed = h
        .get("/rest/juuzCreateApiKey?u=lou&p=loupw&f=json&c=t&name=x")
        .await
        .json();
    assert_eq!(allowed["status"], "ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn replaygain_tags_reach_the_play_buttons() {
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::{Accessor, ItemKey};

    let h = Harness::new();
    let path = make_track(
        h.music.path(),
        "Alpha/First/01 - One.wav",
        95,
        &Tags {
            title: Some("One"),
            artist: Some("Alpha"),
            album: Some("First"),
            track: Some(1),
            ..Default::default()
        },
    );
    let mut file = lofty::read_from_path(&path).unwrap();
    let tag = file.primary_tag_mut().unwrap();
    tag.insert_text(ItemKey::ReplayGainTrackGain, "-3.20 dB".into());
    assert_eq!(tag.title().as_deref(), Some("One"));
    file.save_to_path(&path, WriteOptions::default()).unwrap();
    make_track(
        h.music.path(),
        "Alpha/First/02 - Two.wav",
        96,
        &Tags {
            title: Some("Two"),
            artist: Some("Alpha"),
            album: Some("First"),
            track: Some(2),
            ..Default::default()
        },
    );
    h.scan().await;
    let admin = login(&h, USER, PASS).await;
    let html = page(&h, "/library?view=songs", Some(&admin)).await.text();
    assert!(html.contains("data-gain=\"-3.20\""), "{html}");
    assert!(
        html.contains("data-gain=\"\""),
        "untagged songs carry no gain"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn web_player_plays_reach_now_playing_and_stats() {
    let h = Harness::new();
    make_track(
        h.music.path(),
        "Alpha/First/01 - One.wav",
        97,
        &Tags {
            title: Some("One"),
            artist: Some("Alpha"),
            album: Some("First"),
            track: Some(1),
            ..Default::default()
        },
    );
    h.scan().await;
    let found = h.ok("search3", "query=one").await["searchResult3"].clone();
    let song = found["song"][0]["id"].as_str().unwrap().to_string();
    let admin = login(&h, USER, PASS).await;

    let anon = post(
        &h,
        &format!("/scrobble?id={song}&submission=true"),
        None,
        "",
    )
    .await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);

    let started = post(
        &h,
        &format!("/scrobble?id={song}&submission=false"),
        Some(&admin),
        "",
    )
    .await;
    assert_eq!(started.status, StatusCode::NO_CONTENT);
    let playing = h.ok("getNowPlaying", "").await;
    assert_eq!(playing["nowPlaying"]["entry"][0]["title"], "One");

    let counted = post(
        &h,
        &format!("/scrobble?id={song}&submission=true"),
        Some(&admin),
        "",
    )
    .await;
    assert_eq!(counted.status, StatusCode::NO_CONTENT);
    let v = h.ok("getSong", &format!("id={song}")).await["song"].clone();
    assert_eq!(v["playCount"], 1, "{v}");
    for kind in ["PLAY", "STOP"] {
        let r = post(
            &h,
            &format!("/playback?type={kind}&id={song}&position=600"),
            Some(&admin),
            "",
        )
        .await;
        assert_eq!(r.status, StatusCode::NO_CONTENT);
    }
    let stats = page(&h, "/", Some(&admin)).await.text();
    assert!(stats.contains("One"), "recent plays should list the song");
}

#[tokio::test(flavor = "multi_thread")]
async fn playback_events_become_sessions_in_the_stats() {
    let h = Harness::new();
    for (i, (name, title)) in [("01 - One.wav", "One"), ("02 - Two.wav", "Two")]
        .into_iter()
        .enumerate()
    {
        make_track(
            h.music.path(),
            &format!("Alpha/First/{name}"),
            100 + i as u32,
            &Tags {
                title: Some(title),
                artist: Some("Alpha"),
                album: Some("First"),
                track: Some(i as u32 + 1),
                ..Default::default()
            },
        );
    }
    h.scan().await;
    let found = h.ok("search3", "query=&songCount=10").await["searchResult3"].clone();
    let id_of = |title: &str| {
        found["song"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["title"] == title)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (one, two) = (id_of("One"), id_of("Two"));
    let admin = login(&h, USER, PASS).await;

    let bad = post(
        &h,
        &format!("/playback?type=NOPE&id={one}"),
        Some(&admin),
        "",
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let anon = post(&h, &format!("/playback?type=PLAY&id={one}"), None, "").await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);

    for (kind, id, position) in [
        ("PLAY", &one, 0),
        ("PAUSE", &one, 300),
        ("RESUME", &one, 300),
        ("STOP", &one, 600),
        ("PLAY", &two, 0),
        ("PLAY", &one, 0),
        ("STOP", &one, 600),
    ] {
        let r = post(
            &h,
            &format!("/playback?type={kind}&id={id}&position={position}"),
            Some(&admin),
            "",
        )
        .await;
        assert_eq!(r.status, StatusCode::NO_CONTENT, "{kind}");
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }

    let html = page(&h, "/", Some(&admin)).await.text();
    assert!(html.contains("66%"), "two of three plays completed: {html}");
    assert!(html.contains("33%"), "one of three plays skipped: {html}");
    assert!(html.contains("plays by hour"));
}

#[tokio::test(flavor = "multi_thread")]
async fn library_songs_are_chunked_and_sortable() {
    let h = Harness::new();
    for (i, title) in ["Bravo", "Alpha", "Charlie"].iter().enumerate() {
        make_track(
            h.music.path(),
            &format!("Artist/Album/{:02} - {title}.wav", i + 1),
            120 + i as u32,
            &Tags {
                title: Some(title),
                artist: Some("Artist"),
                album: Some("Album"),
                track: Some(i as u32 + 1),
                ..Default::default()
            },
        );
    }
    h.scan().await;
    let admin = login(&h, USER, PASS).await;

    let all = page(&h, "/library?view=songs", Some(&admin)).await.text();
    assert!(all.contains("data-total=\"3\""), "{all}");
    let titles = |html: &str| -> Vec<String> {
        html.split("<strong>")
            .skip(1)
            .map(|s| s.split("</strong>").next().unwrap().to_string())
            .collect()
    };
    assert_eq!(titles(&all), ["Bravo", "Alpha", "Charlie"]);

    let sorted = page(&h, "/library?view=songs&sort=title&dir=asc", Some(&admin))
        .await
        .text();
    assert_eq!(titles(&sorted), ["Alpha", "Bravo", "Charlie"]);
    let reversed = page(&h, "/library?view=songs&sort=title&dir=desc", Some(&admin))
        .await
        .text();
    assert_eq!(titles(&reversed), ["Charlie", "Bravo", "Alpha"]);

    let window = page(
        &h,
        "/library?view=songs&sort=title&dir=asc&offset=1&limit=1",
        Some(&admin),
    )
    .await
    .text();
    assert_eq!(titles(&window), ["Bravo"]);
    assert!(window.contains("data-total=\"3\""));
    assert!(window.contains("data-offset=\"1\""));

    let filtered = page(&h, "/library?view=songs&q=alp", Some(&admin))
        .await
        .text();
    assert!(filtered.contains("data-total=\"1\""));
    assert_eq!(titles(&filtered), ["Alpha"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn album_page_shows_its_genres() {
    let h = Harness::new();
    make_track(
        h.music.path(),
        "Artist/Album/01 - One.wav",
        140,
        &Tags {
            title: Some("One"),
            artist: Some("Artist"),
            album: Some("Album"),
            track: Some(1),
            genre: Some("Shoegaze"),
            ..Default::default()
        },
    );
    h.scan().await;
    let found = h.ok("search3", "query=album").await["searchResult3"].clone();
    let album = found["album"][0]["id"].as_str().unwrap().to_string();
    let admin = login(&h, USER, PASS).await;
    let html = page(
        &h,
        &format!("/library?view=songs&album={album}"),
        Some(&admin),
    )
    .await
    .text();
    assert!(html.contains("Shoegaze"), "{html}");
    let grid = page(&h, "/library?view=albums", Some(&admin)).await.text();
    assert!(!grid.contains("Shoegaze"));
}

#[tokio::test]
async fn artist_pages_link_albums_and_songs() {
    let h = Harness::new();
    make_track(
        h.music.path(),
        "Band/Record/01 - One.wav",
        140,
        &Tags {
            title: Some("One"),
            artist: Some("Band"),
            album: Some("Record"),
            track: Some(1),
            ..Default::default()
        },
    );
    h.scan().await;
    let found = h.ok("search3", "query=band").await["searchResult3"].clone();
    let artist = found["artist"][0]["id"].as_str().unwrap().to_string();
    let album = h.ok("search3", "query=record").await["searchResult3"]["album"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let admin = login(&h, USER, PASS).await;
    let list = page(&h, "/library?view=artists", Some(&admin)).await.text();
    assert!(list.contains("Band"), "{list}");
    assert!(list.contains(&format!("artist={artist}")), "{list}");
    let detail = page(
        &h,
        &format!("/library?view=artists&artist={artist}"),
        Some(&admin),
    )
    .await
    .text();
    assert!(detail.contains("Record"), "{detail}");
    assert!(detail.contains(&format!("album={album}")), "{detail}");
    let tracks = page(
        &h,
        &format!("/library?view=songs&album={album}"),
        Some(&admin),
    )
    .await
    .text();
    assert!(tracks.contains(&format!("artist={artist}")), "{tracks}");
    let songs = page(&h, "/library?view=songs", Some(&admin)).await.text();
    assert!(songs.contains(&format!("artist={artist}")), "{songs}");
    assert!(songs.contains(&format!("album={album}")), "{songs}");
}
