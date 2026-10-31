use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::context::try_app_context;
use topcoat::cookie::{Cookies, cookies};
use topcoat::icon::icon;
use topcoat::router::content::Form;
use topcoat::router::{HeaderName, HeaderValue, Slot, header, layout, page, query_params, route};
use topcoat::runtime::{RuntimeSetup, link_attrs, prefetch_mode};
use topcoat::view::{Attributes, Child, View, class, component, view};

use super::auth::{current, fail, require, require_admin, require_role, state};
use super::data::{self, Account, Admin, Playing, Range, Ranked, Recent, Stats};
use super::icons::sheet;
use super::libraries::libraries_panel;
use super::library::{album_link, artist_link, credits};
use super::nav::Page;
use super::notice::Notice;
use crate::auth as crypto;
use crate::db::users::{self, User};

const STYLE: &str = include_str!("style.css");
const FONT: &[u8] = include_bytes!("assets/DepartureMono.woff2");
const LOGO: &str = include_str!("assets/logo-text.svg");
const FAVICON: &[u8] = include_bytes!("assets/favicon.ico");
const CLIENT: &str = include_str!("client.js");

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
pub(super) struct Filters {
    range: Range,
    everyone: bool,
    admin: bool,
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
struct AccountBoard {
    me: User,
    account: Account,
    lastfm_ready: bool,
    reveal: Reveal,
    tz: i64,
}

#[derive(Clone)]
struct ServerBoard {
    me: User,
    libraries: Vec<super::libraries::Row>,
    scan: Scan,
    config: Vec<(&'static str, String)>,
    jukebox: crate::jukebox::Outputs,
    tz: i64,
}

#[query_params(error = bad_request)]
struct Query {
    n: Option<String>,
    range: Option<String>,
    scope: Option<String>,
}

pub(super) fn timezone(cx: &Cx) -> i64 {
    cookies(cx)
        .get("juuz_tz")
        .and_then(|c| c.value().parse::<i64>().ok())
        .filter(|m| (-14 * 60..=14 * 60).contains(m))
        .map_or(0, |m| m * 60)
}

fn onoff(v: bool) -> String {
    if v { "on" } else { "off" }.to_string()
}

pub(super) fn notice_of(cx: &Cx) -> Result<Option<Notice>> {
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
        .playing_now()
        .into_iter()
        .filter(|n| scope.as_deref().is_none_or(|s| s == n.user_id))
        .map(|n| (n.song_id, n.username, n.player))
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

#[route(GET "/logo.svg")]
async fn logo() -> Result<([(HeaderName, HeaderValue); 2], &'static str)> {
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("image/svg+xml"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=604800"),
            ),
        ],
        LOGO,
    ))
}

#[route(GET "/favicon.ico")]
async fn favicon() -> Result<([(HeaderName, HeaderValue); 2], &'static [u8])> {
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("image/x-icon"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            ),
        ],
        FAVICON,
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

fn logo_version() -> u64 {
    xxhash_rust::xxh3::xxh3_64(LOGO.as_bytes())
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
                <title>"Juuz"</title>
                <link rel="icon" href="/favicon.ico" sizes="any">
                <link rel="preload" href="/departure-mono.woff2" as="font" type="font/woff2" crossorigin="">
                <link rel="stylesheet" href=(format!("/juuz.css?v={:x}", style_version()))>
                <script src=(format!("/client.js?v={:x}", client_version()))></script>
                <script src=(format!("/player.js?v={:x}", super::library::script_version())) defer=""></script>
                if runtime_on(cx) { topcoat::runtime::script() }
            </head>
            <body>
                (slot)
                sheet()
            </body>
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
        reveal: Reveal::None,
        tz: timezone(cx),
    };
    Ok(view! { account_view(board: board, notice: notice) })
}

#[page("/users")]
async fn users_page(cx: &Cx) -> Result<impl View> {
    let me = require_admin(cx).await?;
    let notice = notice_of(cx)?;
    let admin = state(cx).db.run(|c| data::admin(c)).await.map_err(fail)?;
    Ok(view! { users_view(me: me, admin: admin, notice: notice) })
}

#[page("/server")]
async fn server_page(cx: &Cx) -> Result<impl View> {
    let me = require_role(cx, |u| u.can_server() || u.can_scan() || u.can_library()).await?;
    let notice = notice_of(cx)?;
    let status = state(cx).scanner.status().map_err(fail)?;
    let libraries = state(cx)
        .db
        .run(|c| super::libraries::rows(c))
        .await
        .map_err(fail)?;
    let board = ServerBoard {
        me,
        libraries,
        scan: Scan {
            scanning: status.scanning,
            count: status.count,
            last_scan: status.last_scan,
        },
        config: config_rows(cx),
        jukebox: state(cx).jukebox.outputs().await,
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
    let me = require_role(cx, User::can_keys).await?;
    let st = state(cx);
    let name = form.name.trim().chars().take(80).collect::<String>();
    let key = crypto::generate_api_key();
    let hash = crypto::hash_api_key(&key);
    let (uid, label) = (me.id.clone(), name.clone());
    st.db
        .run(move |c| Ok(users::create_api_key(c, &uid, &label, &hash)?))
        .await
        .map_err(fail)?;
    let viewer = me.clone();
    let account = st
        .db
        .run(move |c| data::account(c, &viewer))
        .await
        .map_err(fail)?;
    let board = AccountBoard {
        me,
        account,
        lastfm_ready: st.lastfm.is_some(),
        reveal: Reveal::Key { name, key },
        tz: timezone(cx),
    };
    Ok(view! { account_view(board: board, notice: Some(Notice::Saved)) })
}

pub fn routes(b: topcoat::router::RouterBuilder) -> topcoat::router::RouterBuilder {
    b.layout(frame)
        .page(home)
        .page(account_page)
        .page(users_page)
        .page(server_page)
        .page(create_key)
        .route(stylesheet)
        .route(font)
        .route(logo)
        .route(favicon)
        .route(client_script)
}

fn duration(ms: i64) -> String {
    let mins = ms / 60_000;
    match (mins / 1440, (mins / 60) % 24, mins % 60) {
        (0, 0, 0) => format!("{}s", ms / 1000),
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

pub(super) fn ago(ts: i64, tz: i64) -> String {
    let secs = (crate::db::now() - ts).max(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => data::date_label(ts, tz),
    }
}

fn bar(value: i64, max: i64) -> String {
    if value <= 0 || max <= 0 {
        return "width: 0".to_string();
    }
    format!("width: {:.3}%", value as f64 * 100.0 / max as f64)
}

fn wedge(hour: usize, plays: i64, peak: i64) -> String {
    use std::f64::consts::TAU;
    const HOLE: f64 = 0.46;
    const STUB: f64 = 0.05;
    const GAP: f64 = 0.018;
    const STEPS: usize = 8;
    let frac = if peak > 0 {
        plays as f64 / peak as f64
    } else {
        0.0
    };
    let outer = HOLE + STUB + (1.0 - HOLE - STUB) * frac;
    let width = TAU / 24.0;
    let start = hour as f64 * width + GAP - TAU / 4.0;
    let end = (hour + 1) as f64 * width - GAP - TAU / 4.0;
    let point = |radius: f64, angle: f64| {
        format!(
            "{:.2}% {:.2}%",
            50.0 + radius * 50.0 * angle.cos(),
            50.0 + radius * 50.0 * angle.sin()
        )
    };
    let mut points: Vec<String> = (0..=STEPS)
        .map(|i| point(outer, start + (end - start) * i as f64 / STEPS as f64))
        .collect();
    points.extend(
        (0..=STEPS)
            .rev()
            .map(|i| point(HOLE, start + (end - start) * i as f64 / STEPS as f64)),
    );
    let fade = if plays > 0 { "" } else { "; opacity: 0.25" };
    format!("clip-path: polygon({}){}", points.join(", "), fade)
}

fn flags(u: &User) -> [(&'static str, &'static str, bool); 17] {
    [
        ("admin", "admin", u.admin),
        ("settings", "settings", u.settings || u.admin),
        ("stream", "stream", u.stream || u.admin),
        ("download", "download", u.download || u.admin),
        ("upload", "upload", u.upload || u.admin),
        ("playlist", "playlists", u.playlist || u.admin),
        ("cover_art", "cover art", u.cover_art || u.admin),
        ("comment", "comments", u.comment || u.admin),
        ("podcast", "podcasts", u.podcast || u.admin),
        ("share", "sharing", u.share || u.admin),
        ("jukebox", "jukebox", u.jukebox || u.admin),
        ("scan", "library scans", u.scan || u.admin),
        ("server", "server page", u.server || u.admin),
        ("radio", "internet radio", u.radio || u.admin),
        ("delete", "delete songs", u.delete || u.admin),
        ("library", "libraries", u.library || u.admin),
        ("catalogue", "catalogue", u.catalogue || u.admin),
    ]
}

fn view_link(range: Range, everyone: bool) -> String {
    let scope = if everyone { "&scope=all" } else { "" };
    format!("/?range={}{scope}", range.code())
}

#[component]
pub(super) async fn anchor(
    cx: &Cx,
    href: String,
    class: &str,
    label: &str,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    let attrs = if runtime_on(cx) {
        link_attrs(cx, href, prefetch_mode(cx))
    } else {
        let mut plain = Attributes::with_capacity(1);
        plain.insert(cx, "href", href);
        plain
    };
    Ok(view! {
        <a (attrs) class=(class) aria-label=(label)>(child)</a>
    })
}

#[component]
pub(super) async fn jump(
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
        <a (attrs) data-key=(key) class=(class!("link", "current" if on))>(label)</a>
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
pub(super) async fn shell(
    page: Option<Page>,
    name: &str,
    me: Option<User>,
    notice: Option<Notice>,
    #[default] filters: Option<Filters>,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    let authed = !name.is_empty();
    let pages: Vec<Page> = me
        .as_ref()
        .map(|u| Page::EACH.into_iter().filter(|p| p.visible(u)).collect())
        .unwrap_or_default();
    Ok(view! {
        <div class=(class!("screen", "guest" if !authed))>
            <header>
                <h1><img src=(format!("/logo.svg?v={:x}", logo_version())) alt="juuz"></h1>
                if authed {
                    <div class="session">
                        <span>(name)</span>
                        <form method="post" action="/logout"><button class="ghost">"sign out"</button></form>
                    </div>
                }
            </header>
            if authed {
                <div class="bar">
                    <nav class="tabs" aria-label="pages">
                        for entry in pages.iter().copied() {
                            jump(href: entry.path().to_string(), label: entry.label(), key: entry.key().to_string(), on: Some(entry) == page)
                        }
                    </nav>
                    match filters {
                        Some(filters) => filter_bar(filters: filters),
                        None => "",
                    }
                </div>
            }
            banner(notice: notice)
            <main>(child)</main>
        </div>
    })
}

#[component]
async fn filter_bar(filters: Filters) -> Result<impl View> {
    Ok(view! {
        <nav class="filters" aria-label="filters">
            <div class="group" role="group" aria-label="range">
                for range in Range::EACH {
                    jump(href: view_link(range, filters.everyone), label: range.label(), on: range == filters.range)
                }
            </div>
            if filters.admin {
                <div class="group" role="group" aria-label="view">
                    jump(href: view_link(filters.range, false), label: "just me", on: !filters.everyone)
                    jump(href: view_link(filters.range, true), label: "everyone", on: filters.everyone)
                </div>
            }
        </nav>
    })
}

#[component]
pub(super) async fn panel(
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
pub(super) async fn field(
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
            <span class="caption">(label)</span>
            <input name=(name) type=(kind) autocomplete=(auto) value=(value) inputmode=(mode) required=(required) autofocus=(focus)>
        </label>
    })
}

#[component]
pub(super) async fn dropdown(
    name: &str,
    current: &str,
    options: &[(String, String)],
) -> Result<impl View> {
    let label = options
        .iter()
        .find(|(value, _)| value == current)
        .map(|(_, label)| label.as_str())
        .unwrap_or("");
    Ok(view! {
        <div class="select" data-select="">
            <input type="hidden" name=(name) value=(current)>
            <button type="button" class="select-trigger" aria-haspopup="listbox" aria-expanded="false">
                <span class="value">(label)</span>
                icon(data: super::icons::CHEVRON_DOWN_ICON)
            </button>
            <div class="options" role="listbox" hidden=(true)>
                for (value, text) in options.iter() {
                    <button type="button" role="option" data-value=(value.as_str()) aria-selected=(if value == current { "true" } else { "false" })>(text.as_str())</button>
                }
            </div>
        </div>
    })
}

#[component]
pub(super) async fn check(
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
            me: None,
            notice: notice,
            panel(
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
    let widths: Vec<String> = board.stats.days.iter().map(|d| bar(d.plays, top)).collect();
    let peak = board.stats.hours.iter().map(|d| d.plays).max().unwrap_or(0);
    let wedges: Vec<String> = (0..board.stats.hours.len())
        .map(|h| wedge(h, board.stats.hours[h].plays, peak))
        .collect();
    let busiest = board
        .stats
        .hours
        .iter()
        .filter(|d| d.plays == peak && peak > 0)
        .map(|d| d.label.clone())
        .next()
        .unwrap_or_else(|| "-".to_string());
    let share = |part: i64| {
        if board.stats.plays > 0 {
            format!("{}%", part * 100 / board.stats.plays)
        } else {
            "0%".to_string()
        }
    };
    let links = Page::Library.visible(&board.me);
    let completed = share(board.stats.completed);
    let skipped = share(board.stats.skipped);
    let average = if board.stats.plays > 0 {
        let secs = board.stats.listened_ms / board.stats.plays / 1000;
        format!("{}:{:02}", secs / 60, secs % 60)
    } else {
        "0:00".to_string()
    };
    let filters = Some(Filters {
        range: board.range,
        everyone: board.everyone,
        admin: board.me.admin,
    });
    Ok(view! {
        shell(
            page: Some(Page::Stats),
            name: board.me.name.as_str(),
            me: Some(board.me.clone()),
            notice: notice,
            filters: filters,
            <div class="live" data-live="stats">
                panel(
                    title: "overview",
                    note: board.range.label(),
                                        wide: true,
                                        <dl class="tiles">
                        tile(label: "plays", value: board.stats.plays.to_string())
                        tile(label: "listened", value: duration(board.stats.listened_ms))
                        tile(label: "completed", value: completed.clone())
                        tile(label: "skipped", value: skipped.clone())
                        tile(label: "avg listen", value: average.clone())
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
                                    <span class="title">(item.title.as_str()) <small>credits(list: &item.artists, off: !links)</small></span>
                                    <span class="detail">(item.user.as_str()) " · " (item.player.as_str())</span>
                                </li>
                            }
                        </ul>
                    )
                }
                panel(
                    title: chart_title.as_str(),
                    note: board.range.label(),
                    <div class="chart" role="img" aria-label=(chart_title.as_str())>
                        for (day, width) in board.stats.days.iter().zip(widths.iter()) {
                            <div class="day">
                                <span class="date">(day.label.as_str())</span>
                                <span class="bar"><i style=(width.as_str())></i></span>
                                <span class="count">(day.plays)</span>
                            </div>
                        }
                    </div>
                )
                panel(
                    title: "plays by hour",
                    note: board.range.label(),
                    <div class="dial" role="img" aria-label="plays by hour">
                        <div class="ring">
                            for (hour, shape) in board.stats.hours.iter().zip(wedges.iter()) {
                                <i style=(shape.as_str()) title=(format!("{} · {} plays", hour.label, hour.plays))></i>
                            }
                            <span class="peak"><small>"peak"</small>(busiest.as_str())</span>
                        </div>
                        <b class="mark top">"00"</b>
                        <b class="mark right">"06"</b>
                        <b class="mark bottom">"12"</b>
                        <b class="mark left">"18"</b>
                    </div>
                )
                recent_list(items: &board.stats.recent, everyone: board.everyone, tz: board.tz, links: links)
                ranked_list(title: "top songs", kind: "song", items: &board.stats.songs, links: links)
                ranked_list(title: "top albums", kind: "album", items: &board.stats.albums, links: links)
                ranked_list(title: "top artists", kind: "artist", items: &board.stats.artists, links: links)
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
async fn ranked_list(title: &str, kind: &str, items: &[Ranked], links: bool) -> Result<impl View> {
    Ok(view! {
        panel(
            title: title,
            if items.is_empty() {
                <p class="muted">"nothing yet"</p>
            } else {
                <ol class="ranked">
                    for item in items.iter() {
                        <li>
                            <span class="title">
                                if kind == "album" {
                                    album_link(id: item.id.as_str(), title: item.name.as_str(), off: !links)
                                } else if kind == "artist" {
                                    artist_link(id: item.id.as_str(), name: item.name.as_str(), off: !links)
                                } else {
                                    (item.name.as_str())
                                }
                                if !item.sub.is_empty() { <small>credits(list: &item.sub, off: !links)</small> }
                            </span>
                            <span class="detail">(duration(item.listened_ms))</span>
                            <span class="count">(item.plays)</span>
                        </li>
                    }
                </ol>
            }
        )
    })
}

#[component]
async fn recent_list(items: &[Recent], everyone: bool, tz: i64, links: bool) -> Result<impl View> {
    Ok(view! {
        panel(
            title: "recent plays",
            if items.is_empty() {
                <p class="muted">"nothing yet"</p>
            } else {
                <ul>
                    for item in items.iter() {
                        <li>
                            <span class="title">(item.title.as_str()) <small>credits(list: &item.artists, off: !links)</small></span>
                            <span class="detail">
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
            me: Some(board.me.clone()),
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
            if board.me.can_keys() {
            panel(
                title: "api keys",
                                wide: true,
                                match &board.reveal {
                    Reveal::Key { name, key } => <div class="reveal">
                        <p class="caption">"new key: " (name.as_str()) " (shown once, copy it now)"</p>
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
                                <span class="title">
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
                <form method="post" action="/account/apikeys">
                    <label>
                        <span class="caption">"new key label"</span>
                        <input name="name" maxlength="80" autocomplete="off">
                    </label>
                    <button>"create key"</button>
                </form>
            )
            }
        )
    })
}

#[component]
async fn users_view(me: User, admin: Admin, notice: Option<Notice>) -> Result<impl View> {
    Ok(view! {
        shell(
            page: Some(Page::Users),
            name: me.name.as_str(),
            me: Some(me.clone()),
            notice: notice,
            panel(
                title: "users",
                wide: true,
                note: "admin only",
                                <div class="members">
                    for member in admin.members.iter() {
                        member_row(member: member, me: me.id.as_str(), libraries: &admin.libraries)
                    }
                </div>
            )
            panel(
                title: "new user",
                                wide: true,
                                <form method="post" action="/users/create">
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
            me: Some(board.me.clone()),
            notice: notice,
            if board.me.can_server() {
                panel(
                    title: "server",
                    note: "read only",
                    <dl class="facts">
                        for (label, value) in board.config.iter() {
                            <div><dt>(*label)</dt><dd>(value.as_str())</dd></div>
                        }
                    </dl>
                )
            }
            if board.me.can_server() {
                panel(
                    title: "jukebox output",
                    note: "dlna renderer",
                    <p class="muted" data-live="jukebox">
                        match &board.jukebox.current {
                            Some((device, online)) => {
                                (device.name.as_str())
                                if !*online { ", offline" }
                            },
                            None => "none selected",
                        }
                    </p>
                    for device in board.jukebox.devices.iter() {
                        <form method="post" action="/server/jukebox/select">
                            <input type="hidden" name="id" value=(device.udn.as_str())>
                            <button>"use " (device.name.as_str())</button>
                        </form>
                    }
                    <form method="post" action="/server/jukebox/scan">
                        <button>"scan network"</button>
                    </form>
                    <form method="post" action="/server/jukebox/select">
                        field(label: "device description url", name: "url", kind: "url", auto: "off")
                        <button>"use url"</button>
                    </form>
                    if board.jukebox.current.is_some() {
                        <form method="post" action="/server/jukebox/select">
                            <button class="ghost danger">"clear output"</button>
                        </form>
                    }
                )
            }
            if board.me.can_scan() {
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
                    <form method="post" action="/server/scan">
                        check(name: "full", label: "full rescan")
                        <button>"start scan"</button>
                    </form>
                )
            }
            if board.me.can_library() || board.me.can_scan() {
                libraries_panel(me: &board.me, rows: &board.libraries, scanning: board.scan.scanning, tz: board.tz)
            }
        )
    })
}

#[component]
async fn member_row(
    member: &data::Member,
    me: &str,
    libraries: &[crate::db::libraries::Library],
) -> Result<impl View> {
    let user = &member.user;
    let own = user.id == me;
    let current_upload = user
        .upload_library_id
        .map(|id| id.to_string())
        .unwrap_or_default();
    let mut options: Vec<(String, String)> = vec![(String::new(), "first writable".to_string())];
    options.extend(libraries.iter().map(|l| (l.id.to_string(), l.name.clone())));
    let choices: Vec<(String, String, &str, bool, bool)> = libraries
        .iter()
        .map(|l| {
            (
                format!("lib_{}", l.id),
                l.id.to_string(),
                l.name.as_str(),
                user.admin || member.granted.contains(&l.id),
                user.upload_library_id == Some(l.id),
            )
        })
        .collect();
    Ok(view! {
        <details class="member" name="member">
            <summary>
                <strong>(user.name.as_str())</strong>
                if user.admin { <span class="badge">"admin"</span> }
                if own { <span class="badge">"you"</span> }
                <small>(member.plays) " plays"</small>
            </summary>
            <div class="fold">
                <form method="post" action="/users/update">
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
                <form method="post" action="/users/libraries">
                    <input type="hidden" name="username" value=(user.name.as_str())>
                    <div class="roles">
                        for (key, _, label, on, _) in choices.iter() {
                            check(name: key.as_str(), label: *label, checked: *on, disabled: user.admin)
                        }
                    </div>
                    <div class="field">
                        <span class="caption">"upload library"</span>
                        dropdown(name: "upload", current: current_upload.as_str(), options: &options)
                    </div>
                    <button>"save libraries"</button>
                </form>
                if !own {
                    <form method="post" action="/users/delete">
                        <input type="hidden" name="username" value=(user.name.as_str())>
                        <button class="ghost danger">"delete user"</button>
                    </form>
                }
            </div>
        </details>
    })
}
