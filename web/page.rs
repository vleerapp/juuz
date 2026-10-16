use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::cookie::{Cookies, cookies};
use topcoat::router::content::Form;
use topcoat::router::{HeaderName, HeaderValue, Slot, header, layout, page, query_params, route};
use topcoat::view::{Child, View, class, component, view};

use super::auth::{current, fail, require, state};
use super::data::{self, Account, Admin, Day, Playing, Range, Ranked, Recent, Stats};
use super::notice::Notice;
use crate::auth as crypto;
use crate::db::users::{self, User};

const STYLE: &str = include_str!("style.css");
const PAPER: &[u8] = include_bytes!("paper.webp");
const FONT: &[u8] = include_bytes!("Dareo.otf");
const LIVE: &str = include_str!("live.js");

#[derive(Clone)]
enum Reveal {
    None,
    Key { name: String, key: String },
}

#[derive(Clone)]
struct Scan {
    scanning: bool,
    count: u64,
    last_scan: Option<i64>,
}

#[derive(Clone)]
struct Control {
    data: Admin,
    scan: Scan,
    config: Vec<(&'static str, String)>,
}

#[derive(Clone)]
struct Dashboard {
    me: User,
    notice: Option<Notice>,
    reveal: Reveal,
    range: Range,
    everyone: bool,
    stats: Stats,
    playing: Vec<Playing>,
    account: Account,
    lastfm_ready: bool,
    tz: i64,
    control: Option<Control>,
}

#[query_params(error = bad_request)]
struct Query {
    n: Option<String>,
    range: Option<String>,
    scope: Option<String>,
}

fn timezone(cx: &Cx) -> i64 {
    cookies(cx)
        .get("juuz_tz")
        .and_then(|c| c.value().parse::<i64>().ok())
        .filter(|m| (-14 * 60..=14 * 60).contains(m))
        .map_or(0, |m| m * 60)
}

fn onoff(v: bool) -> String {
    if v { "on" } else { "off" }.to_string()
}

fn config_rows(cx: &Cx) -> Vec<(&'static str, String)> {
    let cfg = &state(cx).cfg;
    vec![
        ("version", env!("CARGO_PKG_VERSION").to_string()),
        ("listen", cfg.listen.clone()),
        ("data dir", cfg.data_dir.display().to_string()),
        ("scan on start", onoff(cfg.scan.on_start)),
        ("watch folders", onoff(cfg.scan.watch)),
        ("uploads", onoff(cfg.upload.enabled)),
        ("max upload", bytes(cfg.upload.max_size as i64)),
        (
            "metadata (omm)",
            if cfg.omm.enabled {
                cfg.omm.base_url.clone()
            } else {
                "off".into()
            },
        ),
        ("podcasts", onoff(cfg.podcasts.enabled)),
        (
            "last.fm",
            onoff(cfg.lastfm.api_key.is_some() && cfg.lastfm.secret.is_some()),
        ),
        ("lyrics", cfg.lrclib_url.clone()),
    ]
}

async fn load(
    cx: &Cx,
    me: User,
    notice: Option<Notice>,
    reveal: Reveal,
    range: Range,
    everyone: bool,
) -> Result<Dashboard> {
    let st = state(cx);
    let tz = timezone(cx);
    let everyone = everyone && me.admin;
    let scope = (!everyone).then(|| me.id.clone());
    let entries: Vec<(String, String, String)> = st
        .now_playing
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(uid, _)| scope.as_deref().is_none_or(|s| s == uid.as_str()))
        .map(|(_, n)| (n.song_id.clone(), n.username.clone(), n.player.clone()))
        .collect();
    let (viewer, is_admin) = (me.clone(), me.admin);
    let (stats, playing, account, admin) = st
        .db
        .run(move |c| {
            let stats = data::stats(c, scope.as_deref(), range, tz)?;
            let playing = data::playing(c, entries);
            let account = data::account(c, &viewer)?;
            let admin = if is_admin {
                Some(data::admin(c)?)
            } else {
                None
            };
            Ok((stats, playing, account, admin))
        })
        .await
        .map_err(fail)?;
    let control = match admin {
        Some(data) => {
            let status = st.scanner.status().map_err(fail)?;
            Some(Control {
                data,
                scan: Scan {
                    scanning: status.scanning,
                    count: status.count,
                    last_scan: status.last_scan,
                },
                config: config_rows(cx),
            })
        }
        None => None,
    };
    Ok(Dashboard {
        me,
        notice,
        reveal,
        range,
        everyone,
        stats,
        playing,
        account,
        lastfm_ready: st.lastfm.is_some(),
        tz,
        control,
    })
}

#[route(GET "/juuz.css")]
async fn stylesheet() -> Result<([(HeaderName, HeaderValue); 2], String)> {
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/css; charset=utf-8"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=86400"),
            ),
        ],
        format!("{STYLE}\n{}", super::sketch::css()),
    ))
}

#[route(GET "/paper.webp")]
async fn paper() -> Result<([(HeaderName, HeaderValue); 2], &'static [u8])> {
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/webp")),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=604800"),
            ),
        ],
        PAPER,
    ))
}

#[route(GET "/dareo.otf")]
async fn font() -> Result<([(HeaderName, HeaderValue); 2], &'static [u8])> {
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("font/otf")),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=604800"),
            ),
        ],
        FONT,
    ))
}

#[route(GET "/live.js")]
async fn live_script() -> Result<([(HeaderName, HeaderValue); 2], &'static str)> {
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/javascript; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        LIVE,
    ))
}

#[layout("/")]
async fn frame(slot: Slot<'_>) -> Result<impl View> {
    Ok(view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <meta name="color-scheme" content="light">
                <title>"juuz"</title>
                <link rel="preload" href="/dareo.otf" as="font" type="font/otf" crossorigin="">
                <link rel="preload" href="/paper.webp" as="image" type="image/webp">
                <link rel="stylesheet" href="/juuz.css">
            </head>
            <body>(slot)</body>
        </html>
        ((header::CACHE_CONTROL, HeaderValue::from_static("no-store")))
    })
}

#[page("/")]
async fn home(cx: &Cx) -> Result<impl View> {
    let query = query_params::<Query>(cx)?;
    let notice = Notice::parse(query.n.as_deref());
    let board = match current(cx).await? {
        Some(me) => {
            let range = Range::parse(query.range.as_deref());
            let everyone = query.scope.as_deref() == Some("all");
            Some(load(cx, me, notice, Reveal::None, range, everyone).await?)
        }
        None => None,
    };
    Ok(view! {
        match board {
            Some(board) => dashboard(board: board),
            None => login(notice: notice),
        }
    })
}

#[derive(Deserialize)]
struct KeyForm {
    name: String,
}

#[page(POST "/account/apikeys")]
async fn create_key(cx: &Cx, Form(form): Form<KeyForm>) -> Result<impl View> {
    let me = require(cx).await?;
    let st = state(cx);
    let name = form.name.trim().chars().take(80).collect::<String>();
    let key = crypto::generate_api_key();
    let hash = crypto::hash_api_key(&key);
    let (uid, label) = (me.id.clone(), name.clone());
    st.db
        .run(move |c| Ok(users::create_api_key(c, &uid, &label, &hash)?))
        .await
        .map_err(fail)?;
    let board = load(
        cx,
        me,
        Some(Notice::Saved),
        Reveal::Key { name, key },
        Range::Month,
        false,
    )
    .await?;
    Ok(view! { dashboard(board: board) })
}

pub fn routes(b: topcoat::router::RouterBuilder) -> topcoat::router::RouterBuilder {
    b.layout(frame)
        .page(home)
        .page(create_key)
        .route(stylesheet)
        .route(paper)
        .route(font)
        .route(live_script)
}

fn duration(ms: i64) -> String {
    let mins = ms / 60_000;
    match (mins / 1440, (mins / 60) % 24, mins % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

fn bytes(n: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn ago(ts: i64, tz: i64) -> String {
    let secs = (crate::db::now() - ts).max(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => data::date_label(ts + tz),
    }
}

fn pct(value: i64, max: i64) -> i64 {
    if max <= 0 { 0 } else { value * 100 / max }
}

fn flags(u: &User) -> [(&'static str, &'static str, bool); 11] {
    [
        ("admin", "admin", u.admin),
        ("settings", "settings", u.settings),
        ("stream", "stream", u.stream),
        ("download", "download", u.download),
        ("upload", "upload", u.upload),
        ("playlist", "playlists", u.playlist),
        ("cover_art", "cover art", u.cover_art),
        ("comment", "comments", u.comment),
        ("podcast", "podcasts", u.podcast),
        ("share", "sharing", u.share),
        ("jukebox", "jukebox", u.jukebox),
    ]
}

fn view_link(range: Range, everyone: bool) -> String {
    let scope = if everyone { "&scope=all" } else { "" };
    format!("/?range={}{scope}", range.code())
}

#[component]
async fn banner(notice: Option<Notice>) -> Result<impl View> {
    Ok(view! {
        match notice {
            Some(n) => <p class=(class!("notice", "error" if n.is_error())) role="status">(n.text())</p>,
            None => "",
        }
    })
}

#[component]
async fn panel(
    #[default] title: &str,
    #[default] note: &str,
    #[default] wide: bool,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    Ok(view! {
        <section class=(class!("card", "wide" if wide))>
            if !title.is_empty() {
                <div class="head">
                    <h2>(title)</h2>
                    if !note.is_empty() { <small>(note)</small> }
                </div>
            }
            <div class="body">(child)</div>
        </section>
    })
}

#[component]
async fn login(notice: Option<Notice>) -> Result<impl View> {
    Ok(view! {
        <main class="login">
            <h1>"juuz"</h1>
            banner(notice: notice)
            <form method="post" action="/login" class="form">
                <label>"username"<input name="username" autocomplete="username" required="" autofocus=""></label>
                <label>"password"<input name="password" type="password" autocomplete="current-password" required=""></label>
                <button>"sign in"</button>
            </form>
        </main>
    })
}

#[component]
async fn dashboard(board: Dashboard) -> Result<impl View> {
    Ok(view! {
        <header>
            <h1>"juuz"</h1>
            <span class="who">(board.me.name.as_str())</span>
            <form method="post" action="/logout"><button class="ghost">"sign out"</button></form>
        </header>
        <main>
            <div class="lead">
                banner(notice: board.notice)
                stats_section(board: &board)
            </div>
            <hr class="rule">
            account_section(board: &board)
            keys_section(board: &board)
            match &board.control {
                Some(control) => {
                    <hr class="rule">
                    control_section(control: control, me: board.me.id.as_str(), tz: board.tz)
                },
                None => "",
            }
        </main>
        <script src="/live.js" defer=""></script>
    })
}

#[component]
async fn stats_section(board: &Dashboard) -> Result<impl View> {
    let stats = &board.stats;
    let top = stats.days.iter().map(|d| d.plays).max().unwrap_or(0);
    Ok(view! {
        panel(
            wide: true,
            <div class="live" data-live="stats">
            <div class="toolbar">
                <dl class="tiles">
                    tile(label: "plays", value: stats.plays.to_string())
                    tile(label: "listened", value: duration(stats.listened_ms))
                    tile(label: "songs", value: stats.library.songs.to_string())
                    tile(label: "albums", value: stats.library.albums.to_string())
                    tile(label: "artists", value: stats.library.artists.to_string())
                    tile(label: "library", value: duration(stats.library.duration_ms))
                    tile(label: "size", value: bytes(stats.library.bytes))
                    tile(label: "users", value: stats.library.users.to_string())
                </dl>
                <div class="filters">
                    <nav class="chips">
                        for range in Range::EACH {
                            <a
                                href=(view_link(range, board.everyone))
                                class=(class!("chip", "on" if range == board.range))
                            >(range.label())</a>
                        }
                    </nav>
                    if board.me.admin {
                        <nav class="chips">
                            <a href=(view_link(board.range, false)) class=(class!("chip", "on" if !board.everyone))>"just me"</a>
                            <a href=(view_link(board.range, true)) class=(class!("chip", "on" if board.everyone))>"everyone"</a>
                        </nav>
                    }
                </div>
            </div>
            <div class="chart" aria-label="plays per day, last 14 days">
                for day in stats.days.iter() {
                    chart_day(day: day, top: top)
                }
            </div>
            if !board.playing.is_empty() {
                <div class="group">
                    <h3>"now playing"</h3>
                    <ul class="list">
                        for item in board.playing.iter() {
                            <li>
                                <span>(item.title.as_str()) <small>(item.artist.as_str())</small></span>
                                <small>(item.user.as_str()) " · " (item.player.as_str())</small>
                            </li>
                        }
                    </ul>
                </div>
            }
            <div class="cols">
                ranked_list(title: "top songs", items: &stats.songs)
                ranked_list(title: "top albums", items: &stats.albums)
                ranked_list(title: "top artists", items: &stats.artists)
                recent_list(items: &stats.recent, everyone: board.everyone, tz: board.tz)
            </div>
            </div>
        )
    })
}

#[component]
async fn tile(label: &str, value: String) -> Result<impl View> {
    Ok(view! {
        <div class="tile"><dd>(value)</dd><dt>(label)</dt></div>
    })
}

#[component]
async fn chart_day(day: &Day, top: i64) -> Result<impl View> {
    Ok(view! {
        <div class="col" title=(format!("{}: {}", day.label, day.plays))>
            <div class="bar" style=(format!("height:{}%", pct(day.plays, top).max(2)))></div>
            <span>(day.label.as_str())</span>
        </div>
    })
}

#[component]
async fn ranked_list(title: &str, items: &[Ranked]) -> Result<impl View> {
    Ok(view! {
        <div class="group">
            <h3>(title)</h3>
            if items.is_empty() {
                <p class="muted">"nothing yet"</p>
            } else {
                <ol class="list">
                    for item in items.iter() {
                        <li>
                            <span>(item.name.as_str())
                                if !item.sub.is_empty() { <small>(item.sub.as_str())</small> }
                            </span>
                            <b>(item.plays)</b>
                        </li>
                    }
                </ol>
            }
        </div>
    })
}

#[component]
async fn recent_list(items: &[Recent], everyone: bool, tz: i64) -> Result<impl View> {
    Ok(view! {
        <div class="group">
            <h3>"recent plays"</h3>
            if items.is_empty() {
                <p class="muted">"nothing yet"</p>
            } else {
                <ul class="list">
                    for item in items.iter() {
                        <li>
                            <span>(item.title.as_str()) <small>(item.artist.as_str())</small></span>
                            <small>
                                if everyone { (item.user.as_str()) " · " }
                                (ago(item.played_at, tz))
                            </small>
                        </li>
                    }
                </ul>
            }
        </div>
    })
}

#[component]
async fn account_section(board: &Dashboard) -> Result<impl View> {
    let me = &board.me;
    Ok(view! {
        panel(
            title: "profile",
            <form method="post" action="/account/profile" class="form">
                <label>"username"<input name="username" autocomplete="username" maxlength="254" required="" value=(me.name.as_str())></label>
                <label>"max bitrate (kbps, 0 = unlimited)"
                    <input name="max_bit_rate" inputmode="numeric" pattern="[0-9]*" value=(me.max_bit_rate.to_string())>
                </label>
                <label class="check"><input type="checkbox" name="scrobbling" checked=(me.scrobbling)>"record my plays"</label>
                <button>"save"</button>
            </form>
        )
        panel(
            title: "password",
            <form method="post" action="/account/password" class="form">
                <label>"current"<input name="current" type="password" autocomplete="current-password" required=""></label>
                <label>"new"<input name="new" type="password" autocomplete="new-password" required=""></label>
                <label>"repeat"<input name="confirm" type="password" autocomplete="new-password" required=""></label>
                <button>"change password"</button>
            </form>
        )
        panel(
            title: "last.fm",
            match (&board.account.lastfm, board.lastfm_ready) {
                (Some(name), _) => <form method="post" action="/account/lastfm/unlink" class="row">
                    <span>"linked as " <strong>(name.as_str())</strong></span>
                    <button class="ghost">"unlink"</button>
                </form>,
                (None, true) => <form method="post" action="/account/lastfm/start" class="row">
                    <span class="muted">"not linked"</span>
                    <button>"link"</button>
                </form>,
                (None, false) => <p class="muted">"not configured on this server"</p>,
            }
        )
    })
}

#[component]
async fn keys_section(board: &Dashboard) -> Result<impl View> {
    Ok(view! {
        panel(
            title: "api keys",
            match &board.reveal {
                Reveal::Key { name, key } => <div class="group">
                    <h3>"new key: " (name.as_str()) " (shown once, copy it now)"</h3>
                    <code class="secret">(key.as_str())</code>
                </div>,
                Reveal::None => "",
            }
            if board.account.keys.is_empty() {
                <p class="muted">"no keys"</p>
            } else {
                <ul class="list">
                    for key in board.account.keys.iter() {
                        <li>
                            <span>
                                (if key.name.is_empty() { "unnamed" } else { key.name.as_str() })
                                <small>(ago(key.created, board.tz))</small>
                            </span>
                            <form method="post" action="/account/apikeys/delete">
                                <input type="hidden" name="id" value=(key.id.as_str())>
                                <button class="ghost danger">"delete"</button>
                            </form>
                        </li>
                    }
                </ul>
            }
            <form method="post" action="/account/apikeys" class="row">
                <input name="name" placeholder="label" maxlength="80">
                <button>"create key"</button>
            </form>
        )
    })
}

#[component]
async fn control_section(control: &Control, me: &str, tz: i64) -> Result<impl View> {
    Ok(view! {
        panel(
            title: "users",
            note: "admin only",
            wide: true,
            <div class="members">
                for member in control.data.members.iter() {
                    member_row(member: member, me: me)
                }
            </div>
            <form method="post" action="/admin/users/create" class="form group">
                <h3>"new user"</h3>
                <div class="row">
                    <input name="username" placeholder="username" autocomplete="off" required="">
                    <input name="password" type="password" placeholder="password" autocomplete="new-password" required="">
                    <label class="check"><input type="checkbox" name="admin">"admin"</label>
                    <button>"create user"</button>
                </div>
            </form>
        )
        <hr class="rule">
        panel(
            title: "server",
            note: "read only",
            <dl class="facts">
                for (label, value) in control.config.iter() {
                    <div><dt>(*label)</dt><dd>(value.as_str())</dd></div>
                }
                <div><dt>"music folders"</dt><dd>
                    for folder in control.data.folders.iter() { <span>(folder.as_str())</span> }
                </dd></div>
            </dl>
        )
        panel(
            title: "library scan",
            <p class="muted" data-live="scan">
                if control.scan.scanning { "scanning, " (control.scan.count) " songs so far" }
                else {
                    (control.scan.count) " songs"
                    match control.scan.last_scan {
                        Some(t) => { ", last scan " (ago(t, tz)) },
                        None => "",
                    }
                }
            </p>
            <form method="post" action="/admin/scan" class="row">
                <label class="check"><input type="checkbox" name="full">"full rescan"</label>
                <button>"start scan"</button>
            </form>
        )
    })
}

#[component]
async fn member_row(member: &data::Member, me: &str) -> Result<impl View> {
    let user = &member.user;
    let own = user.id == me;
    Ok(view! {
        <details class="member" name="member">
            <summary>
                <strong>(user.name.as_str())</strong>
                if user.admin { <span class="badge">"admin"</span> }
                if own { <span class="badge">"you"</span> }
                <small>(member.plays) " plays"</small>
            </summary>
            <div class="fold">
                <form method="post" action="/admin/users/update" class="form">
                    <input type="hidden" name="username" value=(user.name.as_str())>
                    <div class="roles">
                        for (field, label, on) in flags(user) {
                            <label class="check">
                                <input type="checkbox" name=(field) checked=(on) disabled=(own && field == "admin")>
                                (label)
                            </label>
                        }
                    </div>
                    if own { <input type="hidden" name="admin" value="on"> }
                    <div class="row">
                        <input name="password" type="password" placeholder="new password (optional)" autocomplete="new-password">
                        <button>"save"</button>
                    </div>
                </form>
                if !own {
                    <form method="post" action="/admin/users/delete">
                        <input type="hidden" name="username" value=(user.name.as_str())>
                        <button class="ghost danger">"delete user"</button>
                    </form>
                }
            </div>
        </details>
    })
}
