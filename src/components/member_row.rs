use crate::components::ui::check::check;
use crate::components::ui::dropdown::dropdown;
use crate::components::ui::field::field;
use crate::server::utils::data;
use crate::server::utils::db::users::User;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

pub fn flags(u: &User) -> [(&'static str, &'static str, bool); 16] {
    [
        ("admin", "admin", u.admin),
        ("settings", "settings", u.settings || u.admin),
        ("stream", "stream", u.stream || u.admin),
        ("download", "download", u.download || u.admin),
        ("upload", "upload", u.upload || u.admin),
        ("playlist", "playlists", u.playlist || u.admin),
        ("cover_art", "cover art", u.cover_art || u.admin),
        ("comment", "comments", u.comment || u.admin),
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

#[component]
pub async fn member_row(
    cx: &Cx,
    member: &data::Member,
    me: &str,
    libraries: &[crate::server::utils::db::libraries::Library],
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
                <form method="post" action="/users/update" data-auto="">
                    <input type="hidden" name="username" value=(user.name.as_str())>
                    <div class="roles">
                        for (role, label, on) in flags(user) {
                            check(name: role, label: label, checked: on, disabled: own && role == "admin")
                        }
                    </div>
                    if own { <input type="hidden" name="admin" value="on"> }
                    field(label: "new password (optional)", name: "password", kind: "password", auto: "new-password")
                </form>
                <form method="post" action="/users/libraries" data-auto="">
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
