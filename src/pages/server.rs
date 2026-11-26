use crate::components::libraries_panel::libraries_panel;
use crate::components::ui::check::check;
use crate::components::ui::nav::Page;
use crate::components::ui::panel::panel;
use crate::composables::use_libraries::{Row, rows};
use crate::composables::use_notice::notice_of;
use crate::composables::use_timezone::timezone;
use crate::layouts::shell::shell;
use crate::server::utils::db::users::User;
use crate::server::utils::format::{ago, bytes};
use crate::server::utils::notice::Notice;
use crate::server::utils::session::{fail, require_role, state};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::page;
use topcoat::view::{View, component, view};

#[derive(Clone)]
pub struct Scan {
    pub scanning: bool,
    pub count: u64,
    pub last_scan: Option<i64>,
}

#[derive(Clone)]
pub struct ServerBoard {
    pub me: User,
    pub libraries: Vec<Row>,
    pub scan: Scan,
    pub config: Vec<(&'static str, String)>,
    pub tz: i64,
}

pub fn onoff(v: bool) -> String {
    if v { "on" } else { "off" }.to_string()
}

pub fn config_rows(cx: &Cx) -> Vec<(&'static str, String)> {
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
        (
            "last.fm",
            onoff(cfg.lastfm.api_key.is_some() && cfg.lastfm.secret.is_some()),
        ),
        ("lyrics", cfg.lrclib_url.clone()),
    ]
}

#[page("/server")]
pub async fn server_page(cx: &Cx) -> Result<impl View> {
    let me = require_role(cx, |u| {
        u.can_server() || u.can_scan() || u.can_library() || u.can_delete()
    })
    .await?;
    let notice = notice_of(cx)?;
    let status = state(cx).scanner.status().map_err(fail)?;
    let libraries = state(cx).db.run(|c| rows(c)).await.map_err(fail)?;
    let board = ServerBoard {
        me,
        libraries,
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

#[component]
pub async fn server_view(cx: &Cx, board: ServerBoard, notice: Option<Notice>) -> Result<impl View> {
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
            if board.me.can_scan() {
                panel(
                    title: "library scan",
                    <p class="muted" data-live="scan" data-topic="3">
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
            if board.me.can_catalogue() {
                panel(
                    title: "metadata",
                    <p class="muted">"refetches every matched song, album and artist and retries unmatched ones. manual clears stay cleared."</p>
                    <form method="post" action="/server/metadata/refetch">
                        <button>"refetch all metadata"</button>
                    </form>
                )
            }
            if board.me.can_library() || board.me.can_scan() || board.me.can_delete() {
                libraries_panel(me: &board.me, rows: &board.libraries, scanning: board.scan.scanning, tz: board.tz)
            }
        )
    })
}
