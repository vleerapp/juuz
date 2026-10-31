use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::RouterBuilder;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;
use topcoat::view::{View, component, view};

use super::auth::{fail, require_admin, require_role, state};
use super::nav::Page;
use super::notice::{Notice, back};
use super::page::{ago, check, field, panel};
use crate::db::libraries::{self, Invalid, Library};
use crate::db::users::User;
use crate::scanner::index;

#[derive(Clone)]
pub(super) struct Row {
    library: Library,
    songs: i64,
    last_seen: Option<i64>,
}

pub(super) fn rows(c: &rusqlite::Connection) -> Result<Vec<Row>, crate::error::ApiError> {
    let mut stmt = c.prepare(
        "SELECT library_id, COUNT(*), MAX(indexed_at) / 1000 FROM songs GROUP BY library_id",
    )?;
    let counts: std::collections::HashMap<i64, (i64, Option<i64>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(libraries::list(c)?
        .into_iter()
        .map(|library| {
            let (songs, last_seen) = counts.get(&library.id).copied().unwrap_or((0, None));
            Row {
                library,
                songs,
                last_seen,
            }
        })
        .collect())
}

#[component]
pub(super) async fn libraries_panel(
    me: &User,
    rows: &[Row],
    scanning: bool,
    tz: i64,
) -> Result<impl View> {
    Ok(view! {
        panel(
            title: "libraries",
            wide: true,
            note: if scanning { "scanning" } else { "" },
            <div class="members">
                for row in rows.iter() {
                    library_row(row: row, me: me, tz: tz)
                }
            </div>
            if me.can_library() {
                <form method="post" action="/server/libraries/create">
                    field(label: "name", name: "name", kind: "text", auto: "off", required: true)
                    field(label: "path (mount it outside every other library)", name: "path", kind: "text", auto: "off", required: true)
                    check(name: "writable", label: "uploads allowed", checked: true)
                    <button>"add library"</button>
                </form>
            }
        )
    })
}

#[component]
async fn library_row(row: &Row, me: &User, tz: i64) -> Result<impl View> {
    let library = &row.library;
    let online = library.online();
    let minutes = (library.scan_interval / 60).to_string();
    let id = library.id.to_string();
    Ok(view! {
        <details class="member" name="member">
            <summary>
                <strong>(library.name.as_str())</strong>
                if !online { <span class="badge">"offline"</span> }
                if !library.writable { <span class="badge">"read only"</span> }
                <small>(row.songs) " songs"</small>
            </summary>
            <div class="fold">
                <p class="muted">
                    (library.path.to_string_lossy().into_owned())
                    match row.last_seen {
                        Some(t) => { ", last change " (ago(t, tz)) },
                        None => "",
                    }
                </p>
                if me.can_library() {
                    <form method="post" action="/server/libraries/update">
                        <input type="hidden" name="id" value=(id.as_str())>
                        field(label: "name", name: "name", kind: "text", auto: "off", value: library.name.as_str(), required: true)
                        field(label: "rescan every (minutes, 0 = never)", name: "minutes", kind: "text", auto: "off", mode: "numeric", value: minutes.as_str())
                        check(name: "writable", label: "uploads allowed", checked: library.writable)
                        <button>"save"</button>
                    </form>
                }
                if me.can_scan() {
                    <form method="post" action="/server/libraries/scan">
                        <input type="hidden" name="id" value=(id.as_str())>
                        check(name: "full", label: "full rescan")
                        <button>"scan"</button>
                    </form>
                }
                if me.admin && library.id != libraries::DEFAULT_ID {
                    <form method="post" action="/server/libraries/delete">
                        <input type="hidden" name="id" value=(id.as_str())>
                        <button class="ghost danger">"remove library"</button>
                    </form>
                }
            </div>
        </details>
    })
}

#[derive(Deserialize)]
struct CreateForm {
    name: String,
    path: String,
    writable: Option<String>,
}

#[route(POST "/server/libraries/create")]
async fn create_library(cx: &Cx, Form(form): Form<CreateForm>) -> Result<SeeOther> {
    require_role(cx, User::can_library).await?;
    let st = state(cx);
    let writable = form.writable.is_some();
    let created = st
        .db
        .run(move |c| {
            Ok(libraries::create(
                c,
                &form.name,
                std::path::Path::new(form.path.trim()),
                writable,
            ))
        })
        .await
        .map_err(fail)?;
    let notice = match created {
        Ok(id) => {
            st.db.refresh_roots().map_err(fail)?;
            st.scanner.start_library(Some(id), false);
            Notice::LibraryAdded
        }
        Err(Invalid::Name) => Notice::Invalid,
        Err(Invalid::Path) => Notice::LibraryPath,
        Err(Invalid::Overlap) => Notice::LibraryOverlap,
        Err(Invalid::Exists) => Notice::LibraryExists,
    };
    Ok(back(Page::Server, notice))
}

#[derive(Deserialize)]
struct UpdateForm {
    id: i64,
    name: String,
    #[serde(default)]
    minutes: String,
    writable: Option<String>,
}

#[route(POST "/server/libraries/update")]
async fn update_library(cx: &Cx, Form(form): Form<UpdateForm>) -> Result<SeeOther> {
    require_role(cx, User::can_library).await?;
    let minutes = match form.minutes.trim() {
        "" => 0,
        v => match v.parse::<i64>() {
            Ok(m) if (0..=525_600).contains(&m) => m,
            _ => return Ok(back(Page::Server, Notice::Invalid)),
        },
    };
    let writable = form.writable.is_some();
    let updated = state(cx)
        .db
        .run(move |c| {
            Ok(libraries::update(
                c,
                form.id,
                &form.name,
                writable,
                minutes * 60,
            ))
        })
        .await
        .map_err(fail)?;
    let notice = match updated {
        Ok(true) => Notice::LibraryUpdated,
        Ok(false) => Notice::Invalid,
        Err(Invalid::Name) => Notice::Invalid,
        Err(_) => Notice::LibraryExists,
    };
    Ok(back(Page::Server, notice))
}

#[derive(Deserialize)]
struct ScanForm {
    id: i64,
    full: Option<String>,
}

#[route(POST "/server/libraries/scan")]
async fn scan_library(cx: &Cx, Form(form): Form<ScanForm>) -> Result<SeeOther> {
    require_role(cx, User::can_scan).await?;
    let started = state(cx)
        .scanner
        .start_library(Some(form.id), form.full.is_some());
    Ok(back(
        Page::Server,
        if started {
            Notice::ScanStarted
        } else {
            Notice::ScanBusy
        },
    ))
}

#[derive(Deserialize)]
struct DeleteForm {
    id: i64,
}

#[route(POST "/server/libraries/delete")]
async fn delete_library(cx: &Cx, Form(form): Form<DeleteForm>) -> Result<SeeOther> {
    require_admin(cx).await?;
    if form.id == libraries::DEFAULT_ID {
        return Ok(back(Page::Server, Notice::Invalid));
    }
    let st = state(cx);
    let removed = st
        .db
        .run(move |c| {
            let removed = libraries::delete(c, form.id)?;
            index::prune_orphans(c)?;
            Ok(removed)
        })
        .await
        .map_err(fail)?;
    st.db.refresh_roots().map_err(fail)?;
    Ok(back(
        Page::Server,
        if removed {
            Notice::LibraryRemoved
        } else {
            Notice::Invalid
        },
    ))
}

pub fn routes(b: RouterBuilder) -> RouterBuilder {
    b.route(create_library)
        .route(update_library)
        .route(scan_library)
        .route(delete_library)
}
