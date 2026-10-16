use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::context::try_app_context;
use topcoat::cookie::{Cookies, cookies};
use topcoat::router::content::Form;
use topcoat::router::{HeaderName, HeaderValue, Slot, header, layout, page, query_params, route};
use topcoat::runtime::{RuntimeSetup, link_attrs, prefetch_mode};
use topcoat::view::{Attributes, Child, View, class, component, view};

use super::auth::{current, fail, require, require_admin, state};
use super::data::{self, Account, Admin, Playing, Range, Ranked, Recent, Stats};
use super::nav::Page;
use super::notice::Notice;
use crate::auth as crypto;
use crate::db::users::{self, User};

const STYLE: &str = include_str!("style.css");
const FONT: &[u8] = include_bytes!("DepartureMono.woff2");
const CLIENT: &str = include_str!("client.js");
const BAR_WIDTH: i64 = 40;
const EIGHTHS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];

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
struct StatsBoard {
    me: User,
    range: Range,
    everyone: bool,
    stats: Stats,
    playing: Vec<Playing>,
    tz: i64,
}

#[derive(Clone)]
struct KeysBoard {
    me: User,
    reveal: Reveal,
    account: Account,
    tz: i64,
}

#[derive(Clone)]
struct AccountBoard {
    me: User,
    account: Account,
    lastfm_ready: bool,
}

#[derive(Clone)]
struct ServerBoard {
    me: User,
    scan: Scan,
    config: Vec<(&'static str, String)>,
    tz: i64,
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

fn notice_of(cx: &Cx) -> Result<Option<Notice>> {
    let query = query_params::<Query>(cx)?;
    Ok(Notice::parse(query.n.as_deref()))
}

fn config_rows(cx: &Cx) -> Vec<(&'static str, String)> {
    let cfg = &state(cx).cfg;
    vec![
        ("version", env!("CARGO_PKG_VERSION").to_string()),
        ("listen", cfg.listen.clone()),
        ("data dir", cfg.data_dir.display().to_string()),
        ("music dir", cfg.music_dir.display().to_string()),
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

async fn stats_board(cx: &Cx, me: User, range: Range, everyone: bool) -> Result<StatsBoard> {
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
    let (stats, playing) = st
        .db
        .run(move |c| {
            let stats = data::stats(c, scope.as_deref(), range, tz)?;
            let playing = data::playing(c, entries);
            Ok((stats, playing))
        })
        .await
        .map_err(fail)?;
    Ok(StatsBoard {
        me,
        range,
        everyone,
        stats,
        playing,
        tz,
    })
}

async fn keys_board(cx: &Cx, me: User, reveal: Reveal) -> Result<KeysBoard> {
    let viewer = me.clone();
    let account = state(cx)
        .db
        .run(move |c| data::account(c, &viewer))
        .await
        .map_err(fail)?;
    Ok(KeysBoard {
        me,
        reveal,
        account,
        tz: timezone(cx),
    })
}

#[route(GET "/juuz.css")]
async fn stylesheet() -> Result<([(HeaderName, HeaderValue); 2], &'static str)> {
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
        STYLE,
    ))
}

#[route(GET "/departure-mono.woff2")]
async fn font() -> Result<([(HeaderName, HeaderValue); 2], &'static [u8])> {
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("font/woff2")),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=604800"),
            ),
        ],
        FONT,
    ))
}

#[route(GET "/client.js")]
async fn client_script() -> Result<([(HeaderName, HeaderValue); 2], &'static str)> {
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/javascript; charset=utf-8"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=86400"),
            ),
        ],
        CLIENT,
    ))
}

fn style_version() -> u64 {
    xxhash_rust::xxh3::xxh3_64(STYLE.as_bytes())
}

fn client_version() -> u64 {
    xxhash_rust::xxh3::xxh3_64(CLIENT.as_bytes())
}

fn runtime_on(cx: &Cx) -> bool {
    try_app_context::<RuntimeSetup>(cx).is_some()
}

#[layout("/")]
async fn frame(cx: &Cx, slot: Slot<'_>) -> Result<impl View> {
    Ok(view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <meta name="color-scheme" content="dark">
                <title>"juuz"</title>
                <link rel="preload" href="/departure-mono.woff2" as="font" type="font/woff2" crossorigin="">
                <link rel="stylesheet" href=(format!("/juuz.css?v={:x}", style_version()))>
                <script src=(format!("/client.js?v={:x}", client_version()))></script>
                if runtime_on(cx) { topcoat::runtime::script() }
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
            Some(stats_board(cx, me, range, everyone).await?)
        }
        None => None,
    };
    Ok(view! {
        match board {
            Some(board) => stats_page(board: board, notice: notice),
            None => login_page(notice: notice),
        }
    })
}

#[page("/account")]
async fn account_page(cx: &Cx) -> Result<impl View> {
    let me = require(cx).await?;
    let notice = notice_of(cx)?;
    let viewer = me.clone();
    let account = state(cx)
        .db
        .run(move |c| data::account(c, &viewer))
        .await
        .map_err(fail)?;
    let board = AccountBoard {
        me,
        account,
        lastfm_ready: state(cx).lastfm.is_some(),
    };
    Ok(view! { account_view(board: board, notice: notice) })
}

#[page("/account/keys")]
async fn keys_page(cx: &Cx) -> Result<impl View> {
    let me = require(cx).await?;
    let notice = notice_of(cx)?;
    let board = keys_board(cx, me, Reveal::None).await?;
    Ok(view! { keys_view(board: board, notice: notice) })
}

#[page("/admin")]
async fn users_page(cx: &Cx) -> Result<impl View> {
    let me = require_admin(cx).await?;
    let notice = notice_of(cx)?;
    let admin = state(cx).db.run(|c| data::admin(c)).await.map_err(fail)?;
    Ok(view! { users_view(me: me, admin: admin, notice: notice) })
}

#[page("/admin/server")]
async fn server_page(cx: &Cx) -> Result<impl View> {
    let me = require_admin(cx).await?;
    let notice = notice_of(cx)?;
    let status = state(cx).scanner.status().map_err(fail)?;
    let board = ServerBoard {
        me,
        scan: Scan {
            scanning: status.scanning,
            count: status.count,
            last_scan: status.last_scan,
        },
        config: config_rows(cx),
        tz: timezone(cx),
    };
    Ok(view! { server_view(board: board, notice: notice) })
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
    let board = keys_board(cx, me, Reveal::Key { name, key }).await?;
    Ok(view! { keys_view(board: board, notice: Some(Notice::Saved)) })
}

pub fn routes(b: topcoat::router::RouterBuilder) -> topcoat::router::RouterBuilder {
    b.layout(frame)
        .page(home)
        .page(account_page)
        .page(keys_page)
        .page(users_page)
        .page(server_page)
        .page(create_key)
        .route(stylesheet)
        .route(font)
        .route(client_script)
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

fn bar(value: i64, max: i64) -> String {
    if value <= 0 || max <= 0 {
        return String::new();
    }
    let eighths = ((value * BAR_WIDTH * 8 + max / 2) / max).max(1) as usize;
    let mut out = "█".repeat(eighths / 8);
    if !eighths.is_multiple_of(8) {
        out.push(EIGHTHS[eighths % 8]);
    }
    out
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
async fn jump(
    cx: &Cx,
    href: String,
    label: &str,
    #[default] key: String,
    #[default] on: bool,
) -> Result<impl View> {
    let attrs = if runtime_on(cx) {
        link_attrs(cx, href, prefetch_mode(cx))
    } else {
        let mut plain = Attributes::with_capacity(1);
        plain.insert(cx, "href", href);
        plain
    };
    Ok(view! {
        <a (attrs) data-key=(key) class=(class!("link", "on" if on))>(label)</a>
    })
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
async fn shell(
    page: Option<Page>,
    name: &str,
    admin: bool,
    notice: Option<Notice>,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    let authed = !name.is_empty();
    Ok(view! {
        <div class="screen">
            <header>
                <h1>"juuz"</h1>
                if authed {
                    <div class="who">
                        <span>(name)</span>
                        <form method="post" action="/logout"><button class="ghost">"sign out"</button></form>
                    </div>
                }
            </header>
            if authed {
                <nav class="tabs" aria-label="pages">
                    for entry in Page::EACH {
                        if admin || !entry.admin() {
                            jump(href: entry.path().to_string(), label: entry.label(), key: entry.key().to_string(), on: Some(entry) == page)
                        }
                    }
                </nav>
            }
            banner(notice: notice)
            <main>(child)</main>
        </div>
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
        <section class=(class!("panel", "wide" if wide))>
            if !title.is_empty() {
                <h2>(title) if !note.is_empty() { <small>(note)</small> }</h2>
            }
            (child)
        </section>
    })
}

#[component]
async fn field(
    label: &str,
    name: &str,
    kind: &str,
    auto: &str,
    #[default] value: &str,
    #[default] mode: &str,
    #[default] required: bool,
    #[default] focus: bool,
) -> Result<impl View> {
    Ok(view! {
        <label>
            <span class="lbl">(label)</span>
            <input name=(name) type=(kind) autocomplete=(auto) value=(value) inputmode=(mode) required=(required) autofocus=(focus)>
        </label>
    })
}

#[component]
async fn check(
    name: &str,
    label: &str,
    #[default] checked: bool,
    #[default] disabled: bool,
) -> Result<impl View> {
    Ok(view! {
        <label class="check">
            <input type="checkbox" name=(name) checked=(checked) disabled=(disabled)>
            (label)
        </label>
    })
}

#[component]
async fn login_page(notice: Option<Notice>) -> Result<impl View> {
    Ok(view! {
        shell(
            page: None,
            name: "",
            admin: false,
            notice: notice,
            panel(
                    title: "sign in",
                    <form method="post" action="/login">
                        field(label: "username", name: "username", kind: "text", auto: "username", required: true, focus: true)
                        field(label: "password", name: "password", kind: "password", auto: "current-password", required: true)
                        <button>"sign in"</button>
                    </form>
                )
        )
    })
}

#[component]
async fn stats_page(board: StatsBoard, notice: Option<Notice>) -> Result<impl View> {
    let top = board.stats.days.iter().map(|d| d.plays).max().unwrap_or(0);
    let chart_title = format!("plays per {}", board.range.unit());
    Ok(view! {
        shell(
            page: Some(Page::Stats),
            name: board.me.name.as_str(),
            admin: board.me.admin,
            notice: notice,
            <nav class="filters" aria-label="filters">
                <div class="group" role="group" aria-label="range">
                    for range in Range::EACH {
                        jump(href: view_link(range, board.everyone), label: range.label(), on: range == board.range)
                    }
                </div>
                if board.me.admin {
                    <div class="group" role="group" aria-label="view">
                        jump(href: view_link(board.range, false), label: "just me", on: !board.everyone)
                        jump(href: view_link(board.range, true), label: "everyone", on: board.everyone)
                    </div>
                }
            </nav>
            <div class="live" data-live="stats">
                panel(
                    title: "overview",
                    note: board.range.label(),
                                        wide: true,
                                        <dl class="tiles">
                        tile(label: "plays", value: board.stats.plays.to_string())
                        tile(label: "listened", value: duration(board.stats.listened_ms))
                        tile(label: "songs", value: board.stats.library.songs.to_string())
                        tile(label: "albums", value: board.stats.library.albums.to_string())
                        tile(label: "artists", value: board.stats.library.artists.to_string())
                        tile(label: "library", value: duration(board.stats.library.duration_ms))
                        tile(label: "size", value: bytes(board.stats.library.bytes))
                        tile(label: "users", value: board.stats.library.users.to_string())
                    </dl>
                )
                if !board.playing.is_empty() {
                    panel(
                        title: "now playing",
                                                wide: true,
                                                <ul>
                            for item in board.playing.iter() {
                                <li>
                                    <span class="t">(item.title.as_str()) <small>(item.artist.as_str())</small></span>
                                    <span class="r">(item.user.as_str()) " · " (item.player.as_str())</span>
                                </li>
                            }
                        </ul>
                    )
                }
                panel(
                    title: chart_title.as_str(),
                    note: board.range.label(),
                    <div class="chart" role="img" aria-label=(chart_title.as_str())>
                        for day in board.stats.days.iter() {
                            <div class="day">
                                <span class="d">(day.label.as_str())</span>
                                <span class="bar">(bar(day.plays, top))</span>
                                <b>(day.plays)</b>
                            </div>
                        }
                    </div>
                )
                recent_list(items: &board.stats.recent, everyone: board.everyone, tz: board.tz)
                ranked_list(title: "top songs", items: &board.stats.songs)
                ranked_list(title: "top albums", items: &board.stats.albums)
                ranked_list(title: "top artists", items: &board.stats.artists)
            </div>
        )
    })
}

#[component]
async fn tile(label: &str, value: String) -> Result<impl View> {
    Ok(view! {
        <div class="tile"><dt>(label)</dt><dd>(value)</dd></div>
    })
}

#[component]
async fn ranked_list(title: &str, items: &[Ranked]) -> Result<impl View> {
    Ok(view! {
        panel(
            title: title,
            if items.is_empty() {
                <p class="muted">"nothing yet"</p>
            } else {
                <ol>
                    for item in items.iter() {
                        <li>
                            <span class="t">(item.name.as_str())
                                if !item.sub.is_empty() { <small>(item.sub.as_str())</small> }
                            </span>
                            <b>(item.plays)</b>
                        </li>
                    }
                </ol>
            }
        )
    })
}

#[component]
async fn recent_list(items: &[Recent], everyone: bool, tz: i64) -> Result<impl View> {
    Ok(view! {
        panel(
            title: "recent plays",
            if items.is_empty() {
                <p class="muted">"nothing yet"</p>
            } else {
                <ul>
                    for item in items.iter() {
                        <li>
                            <span class="t">(item.title.as_str()) <small>(item.artist.as_str())</small></span>
                            <span class="r">
                                if everyone { (item.user.as_str()) " · " }
                                (ago(item.played_at, tz))
                            </span>
                        </li>
                    }
                </ul>
            }
        )
    })
}

#[component]
async fn account_view(board: AccountBoard, notice: Option<Notice>) -> Result<impl View> {
    let rate = board.me.max_bit_rate.to_string();
    Ok(view! {
        shell(
            page: Some(Page::Account),
            name: board.me.name.as_str(),
            admin: board.me.admin,
            notice: notice,
            panel(
                title: "profile",
                <form method="post" action="/account/profile">
                    field(label: "username", name: "username", kind: "text", auto: "username", value: board.me.name.as_str(), required: true)
                    field(label: "max bitrate (kbps, 0 = unlimited)", name: "max_bit_rate", kind: "text", auto: "off", value: rate.as_str(), mode: "numeric")
                    check(name: "scrobbling", label: "record my plays", checked: board.me.scrobbling)
                    <button>"save"</button>
                </form>
            )
            panel(
                title: "password",
                <form method="post" action="/account/password">
                    field(label: "current", name: "current", kind: "password", auto: "current-password", required: true)
                    field(label: "new", name: "new", kind: "password", auto: "new-password", required: true)
                    field(label: "repeat", name: "confirm", kind: "password", auto: "new-password", required: true)
                    <button>"change password"</button>
                </form>
            )
            panel(
                title: "last.fm",
                                wide: true,
                                match (&board.account.lastfm, board.lastfm_ready) {
                    (Some(name), _) => <form method="post" action="/account/lastfm/unlink">
                        <span>"linked as " <strong>(name.as_str())</strong></span>
                        <button class="ghost">"unlink"</button>
                    </form>,
                    (None, true) => <form method="post" action="/account/lastfm/start">
                        <span class="muted">"not linked"</span>
                        <button>"link"</button>
                    </form>,
                    (None, false) => <p class="muted">"not configured on this server"</p>,
                }
            )
        )
    })
}

#[component]
async fn keys_view(board: KeysBoard, notice: Option<Notice>) -> Result<impl View> {
    Ok(view! {
        shell(
            page: Some(Page::Keys),
            name: board.me.name.as_str(),
            admin: board.me.admin,
            notice: notice,
            panel(
                title: "api keys",
                                wide: true,
                                match &board.reveal {
                    Reveal::Key { name, key } => <div class="reveal">
                        <p class="lbl">"new key: " (name.as_str()) " (shown once, copy it now)"</p>
                        <code data-copy="" tabindex="0" class="secret">(key.as_str())</code>
                    </div>,
                    Reveal::None => "",
                }
                if board.account.keys.is_empty() {
                    <p class="muted">"no keys"</p>
                } else {
                    <ul>
                        for key in board.account.keys.iter() {
                            <li>
                                <span class="t">
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
            )
            panel(
                title: "new key",
                                wide: true,
                                <form method="post" action="/account/apikeys">
                    <label>
                        <span class="lbl">"label"</span>
                        <input name="name" maxlength="80" autocomplete="off">
                    </label>
                    <button>"create key"</button>
                </form>
            )
        )
    })
}

#[component]
async fn users_view(me: User, admin: Admin, notice: Option<Notice>) -> Result<impl View> {
    Ok(view! {
        shell(
            page: Some(Page::Users),
            name: me.name.as_str(),
            admin: true,
            notice: notice,
            panel(
                title: "users",
                wide: true,
                note: "admin only",
                                <div class="members">
                    for member in admin.members.iter() {
                        member_row(member: member, me: me.id.as_str())
                    }
                </div>
            )
            panel(
                title: "new user",
                                wide: true,
                                <form method="post" action="/admin/users/create">
                    field(label: "username", name: "username", kind: "text", auto: "off", required: true)
                    field(label: "password", name: "password", kind: "password", auto: "new-password", required: true)
                    check(name: "admin", label: "admin")
                    <button>"create user"</button>
                </form>
            )
        )
    })
}

#[component]
async fn server_view(board: ServerBoard, notice: Option<Notice>) -> Result<impl View> {
    Ok(view! {
        shell(
            page: Some(Page::Server),
            name: board.me.name.as_str(),
            admin: true,
            notice: notice,
            panel(
                title: "server",
                note: "read only",
                <dl class="facts">
                    for (label, value) in board.config.iter() {
                        <div><dt>(*label)</dt><dd>(value.as_str())</dd></div>
                    }
                </dl>
            )
            panel(
                title: "library scan",
                <p class="muted" data-live="scan">
                    if board.scan.scanning { "scanning, " (board.scan.count) " songs so far" }
                    else {
                        (board.scan.count) " songs"
                        match board.scan.last_scan {
                            Some(t) => { ", last scan " (ago(t, board.tz)) },
                            None => "",
                        }
                    }
                </p>
                <form method="post" action="/admin/scan">
                    check(name: "full", label: "full rescan")
                    <button>"start scan"</button>
                </form>
            )
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
                <form method="post" action="/admin/users/update">
                    <input type="hidden" name="username" value=(user.name.as_str())>
                    <div class="roles">
                        for (role, label, on) in flags(user) {
                            check(name: role, label: label, checked: on, disabled: own && role == "admin")
                        }
                    </div>
                    if own { <input type="hidden" name="admin" value="on"> }
                    field(label: "new password (optional)", name: "password", kind: "password", auto: "new-password")
                    <button>"save"</button>
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
