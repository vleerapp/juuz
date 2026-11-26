use crate::server::api::rest::playlists::{self as store, PlaylistRow};
use crate::server::utils::db::users::User;
use crate::server::utils::format::{clock, escape};
use crate::server::utils::notice::Notice;
use crate::server::utils::session::{fail, require_role, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::error::{SeeOther, see_other};

pub const NAME_MAX: usize = 100;

pub const MAX_ADD: usize = 500;

#[derive(Clone)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub comment: String,
    pub detail: String,
    pub cover: bool,
    pub mine: bool,
}

#[derive(Clone, Default)]
pub struct Shelf {
    pub lists: Vec<Playlist>,
    pub open: Option<Playlist>,
    pub slots: Vec<usize>,
}

pub fn playlist(row: &PlaylistRow, me: &User) -> Playlist {
    let mine = row.owner_id == me.id || me.admin;
    let mut parts = vec![format!("{} songs", row.song_count), clock(row.duration_ms)];
    if row.owner_id != me.id {
        parts.push(row.owner.clone());
    }
    if row.public {
        parts.push("public".to_string());
    }
    Playlist {
        id: row.id.clone(),
        name: row.name.clone(),
        comment: row.comment.clone(),
        detail: parts.join(" - "),
        cover: row.song_count > 0,
        mine,
    }
}

pub fn href(id: &str) -> String {
    format!("/?view=playlists&playlist={}", escape(id))
}

pub fn opened(id: &str, notice: Notice) -> SeeOther {
    see_other(format!("{}&n={}", href(id), notice.code()))
}

pub fn listed(notice: Notice) -> SeeOther {
    see_other(format!("/?view=playlists&n={}", notice.code()))
}

pub fn clean(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.chars().count() <= NAME_MAX).then(|| name.to_string())
}

#[derive(Deserialize)]
pub struct MoveForm {
    pub id: String,
    pub song: String,
    pub index: usize,
    pub to: usize,
}

pub async fn shift(cx: &Cx, form: MoveForm) -> Result<bool> {
    let me = require_role(cx, |u| u.playlist || u.admin).await?;
    let id = form.id.clone();
    state(cx)
        .db
        .run(move |c| {
            if store::owned(c, &id, &me).is_err() {
                return Ok(false);
            }
            let mut songs = store::song_ids(c, &id)?;
            if songs.get(form.index) != Some(&form.song) || form.to >= songs.len() {
                return Ok(false);
            }
            let moved = songs.remove(form.index);
            songs.insert(form.to, moved);
            store::write_songs(c, &id, &songs)?;
            Ok(true)
        })
        .await
        .map_err(fail)
}
