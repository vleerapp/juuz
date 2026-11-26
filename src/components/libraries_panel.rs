use crate::components::ui::check::check;
use crate::components::ui::field::field;
use crate::components::ui::panel::panel;
use crate::composables::use_libraries::Row;
use crate::server::utils::db::libraries;
use crate::server::utils::db::users::User;
use crate::server::utils::format::ago;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

#[component]
pub async fn libraries_panel(
    cx: &Cx,
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
pub async fn library_row(cx: &Cx, row: &Row, me: &User, tz: i64) -> Result<impl View> {
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
                    <form method="post" action="/server/libraries/update" data-auto="">
                        <input type="hidden" name="id" value=(id.as_str())>
                        field(label: "name", name: "name", kind: "text", auto: "off", value: library.name.as_str(), required: true)
                        field(label: "rescan every (minutes, 0 = never)", name: "minutes", kind: "text", auto: "off", mode: "numeric", value: minutes.as_str())
                        check(name: "writable", label: "uploads allowed", checked: library.writable)
                    </form>
                }
                if me.can_scan() {
                    <form method="post" action="/server/libraries/scan">
                        <input type="hidden" name="id" value=(id.as_str())>
                        check(name: "full", label: "full rescan")
                        <button>"scan"</button>
                    </form>
                }
                if me.can_delete() && online {
                    <form method="post" action="/server/libraries/missing" data-confirm="">
                        <input type="hidden" name="id" value=(id.as_str())>
                        <button class="ghost danger">"remove missing songs"</button>
                    </form>
                }
                if me.admin && library.id != libraries::DEFAULT_ID {
                    <form method="post" action="/server/libraries/delete" data-confirm="">
                        <input type="hidden" name="id" value=(id.as_str())>
                        <button class="ghost danger">"remove library"</button>
                    </form>
                }
            </div>
        </details>
    })
}
