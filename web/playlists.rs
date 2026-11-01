use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rusqlite::params;
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::icon::icon;
use topcoat::router::RouterBuilder;
use topcoat::router::content::Form;
use topcoat::router::error::{SeeOther, see_other};
use topcoat::router::route;
use topcoat::view::{View, component, view};

use super::auth::{fail, require_role, state};
use super::library::{clock, escape};
use super::nav::Page;
use super::notice::{Notice, back};
use super::page::{anchor, jump};
use crate::api::playlists::{self as store, PlaylistRow};
use crate::db::libraries;
use crate::db::now;
use crate::db::users::User;

const NAME_MAX: usize = 100;
const MAX_ADD: usize = 500;

#[derive(Clone)]
pub(super) struct Playlist {
    pub id: String,
    pub name: String,
    pub comment: String,
    pub detail: String,
    pub cover: bool,
    pub mine: bool,
}

#[derive(Clone, Default)]
pub(super) struct Shelf {
    pub lists: Vec<Playlist>,
    pub open: Option<Playlist>,
    pub slots: Vec<usize>,
}

pub(super) fn playlist(row: &PlaylistRow, me: &User) -> Playlist {
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

pub(super) fn href(id: &str) -> String {
    format!("/library?view=playlists&playlist={}", escape(id))
}

fn opened(id: &str, notice: Notice) -> SeeOther {
    see_other(format!("{}&n={}", href(id), notice.code()))
}

fn listed(notice: Notice) -> SeeOther {
    see_other(format!("/library?view=playlists&n={}", notice.code()))
}

#[component]
pub(super) async fn playlist_tile(list: &Playlist, play: bool) -> Result<impl View> {
    let open = href(&list.id);
    let big = format!("/art?id={}&size=480", escape(&list.id));
    Ok(view! {
        <div class="album-tile">
            if play {
                <button class="thumb big" data-play-list=(list.id.as_str()) aria-label="play playlist">
                    if list.cover {
                        <img class="cover" src=(big.as_str()) alt="" loading="lazy">
                    } else {
                        <span class="cover"></span>
                    }
                    <span class="glyph">icon(data: super::icons::PLAY_ICON)</span>
                </button>
            } else {
                anchor(href: open.clone(), class: "thumb big", label: "open playlist",
                    if list.cover {
                        <img class="cover" src=(big.as_str()) alt="" loading="lazy">
                    } else {
                        <span class="cover"></span>
                    }
                )
            }
            <div class="text">
                <strong>jump(href: open, label: list.name.as_str())</strong>
                <span class="muted">(list.detail.as_str())</span>
            </div>
        </div>
    })
}

#[component]
pub(super) async fn playlist_head(list: &Playlist, play: bool, edit: bool) -> Result<impl View> {
    let big = format!("/art?id={}&size=480", escape(&list.id));
    let editable = edit && list.mine;
    Ok(view! {
        <div class="album-head">
            if play {
                <button class="thumb big" data-play-list=(list.id.as_str()) aria-label="play playlist">
                    if list.cover {
                        <img class="cover" src=(big.as_str()) alt="">
                    } else {
                        <span class="cover"></span>
                    }
                    <span class="glyph">icon(data: super::icons::PLAY_ICON)</span>
                </button>
            } else {
                <span class="thumb big">
                    if list.cover {
                        <img class="cover" src=(big.as_str()) alt="">
                    } else {
                        <span class="cover"></span>
                    }
                </span>
            }
            <div class="text">
                if editable {
                    <form class="rename" method="post" action="/library/playlists/rename" data-rename="">
                        <input type="hidden" name="id" value=(list.id.as_str())>
                        <strong class="title name" tabindex="0" role="button" aria-label="rename playlist" data-rename-open="">(list.name.as_str())</strong>
                        <span class="edit">
                            <input type="text" name="name" value=(list.name.as_str()) maxlength="100" autocomplete="off" required=(true)>
                            <button>"rename"</button>
                        </span>
                    </form>
                } else {
                    <strong class="title">(list.name.as_str())</strong>
                }
                <span class="muted">(list.detail.as_str())</span>
                if !list.comment.is_empty() {
                    <span class="muted">(list.comment.as_str())</span>
                }
                <nav class="acts">
                    if play { <button data-playall="">"play all"</button> }
                    if editable {
                        jump(href: format!("/library?view=songs&add={}", escape(&list.id)), label: "add songs")
                        <form method="post" action="/library/playlists/delete" data-confirm="">
                            <input type="hidden" name="id" value=(list.id.as_str())>
                            <button class="ghost danger">"delete"</button>
                        </form>
                    }
                </nav>
            </div>
        </div>
    })
}

#[component]
pub(super) async fn adding_note(list: &Playlist) -> Result<impl View> {
    Ok(view! {
        <div class="adding">
            <span>"adding to " <strong>(list.name.as_str())</strong></span>
            <nav class="acts">jump(href: href(&list.id), label: "cancel")</nav>
            <form method="post" action="/library/playlists/add" data-pick="">
                <input type="hidden" name="id" value=(list.id.as_str())>
                <input type="hidden" name="songs" value="">
                <button disabled=(true)>"add 0 songs"</button>
            </form>
        </div>
    })
}

#[component]
pub(super) async fn playlist_create() -> Result<impl View> {
    Ok(view! {
        <form class="create" method="post" action="/library/playlists/create">
            <label class="search">
                <input type="text" name="name" placeholder="new playlist" autocomplete="off" required=(true)>
            </label>
            <button>"create"</button>
        </form>
    })
}

fn clean(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.chars().count() <= NAME_MAX).then(|| name.to_string())
}

#[derive(Deserialize)]
struct CreateForm {
    name: String,
}

#[route(POST "/library/playlists/create")]
async fn create(cx: &Cx, Form(form): Form<CreateForm>) -> Result<SeeOther> {
    let me = require_role(cx, |u| u.playlist || u.admin).await?;
    let Some(name) = clean(&form.name) else {
        return Ok(listed(Notice::Invalid));
    };
    let id = crate::ids::random();
    let (made, uid) = (id.clone(), me.id.clone());
    state(cx)
        .db
        .run(move |c| {
            let ts = now();
            c.execute(
                "INSERT INTO playlists(id, owner_id, name, created_at, changed_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![made, uid, name, ts],
            )?;
            Ok(())
        })
        .await
        .map_err(fail)?;
    Ok(opened(&id, Notice::PlaylistSaved))
}

#[derive(Deserialize)]
struct AddForm {
    id: String,
    songs: String,
}

#[route(POST "/library/playlists/add")]
async fn add(cx: &Cx, Form(form): Form<AddForm>) -> Result<SeeOther> {
    let me = require_role(cx, |u| u.playlist || u.admin).await?;
    let mut seen = std::collections::HashSet::new();
    let wanted: Vec<String> = form
        .songs
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && seen.insert(s.to_string()))
        .take(MAX_ADD)
        .map(str::to_string)
        .collect();
    let id = form.id.clone();
    let done = state(cx)
        .db
        .run(move |c| {
            if store::owned(c, &id, &me).is_err() {
                return Ok(false);
            }
            let mut picked = Vec::new();
            for song in wanted {
                if libraries::can_access_song(c, &me.id, &song)? {
                    picked.push(song);
                }
            }
            if picked.is_empty() {
                return Ok(false);
            }
            let mut songs = store::song_ids(c, &id)?;
            songs.extend(picked);
            store::write_songs(c, &id, &songs)?;
            Ok(true)
        })
        .await
        .map_err(fail)?;
    Ok(opened(
        &form.id,
        if done {
            Notice::PlaylistSaved
        } else {
            Notice::Invalid
        },
    ))
}

#[derive(Deserialize)]
struct RenameForm {
    id: String,
    name: String,
}

#[route(POST "/library/playlists/rename")]
async fn rename(cx: &Cx, Form(form): Form<RenameForm>) -> Result<SeeOther> {
    let me = require_role(cx, |u| u.playlist || u.admin).await?;
    let Some(name) = clean(&form.name) else {
        return Ok(opened(&form.id, Notice::Invalid));
    };
    let id = form.id.clone();
    let done = state(cx)
        .db
        .run(move |c| {
            if store::owned(c, &id, &me).is_err() {
                return Ok(false);
            }
            c.execute(
                "UPDATE playlists SET name = ?2, changed_at = ?3 WHERE id = ?1",
                params![id, name, now()],
            )?;
            Ok(true)
        })
        .await
        .map_err(fail)?;
    Ok(if done {
        opened(&form.id, Notice::PlaylistSaved)
    } else {
        back(Page::Library, Notice::Invalid)
    })
}

#[derive(Deserialize)]
struct RemoveForm {
    id: String,
    song: String,
    index: usize,
}

#[route(POST "/library/playlists/remove")]
async fn remove(cx: &Cx, Form(form): Form<RemoveForm>) -> Result<SeeOther> {
    let me = require_role(cx, |u| u.playlist || u.admin).await?;
    let id = form.id.clone();
    let done = state(cx)
        .db
        .run(move |c| {
            if store::owned(c, &id, &me).is_err() {
                return Ok(false);
            }
            let mut songs = store::song_ids(c, &id)?;
            if songs.get(form.index) != Some(&form.song) {
                return Ok(false);
            }
            songs.remove(form.index);
            store::write_songs(c, &id, &songs)?;
            Ok(true)
        })
        .await
        .map_err(fail)?;
    Ok(if done {
        opened(&form.id, Notice::PlaylistSaved)
    } else {
        opened(&form.id, Notice::Invalid)
    })
}

#[derive(Deserialize)]
struct MoveForm {
    id: String,
    song: String,
    index: usize,
    to: usize,
}

async fn shift(cx: &Cx, form: MoveForm) -> Result<bool> {
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

#[route(POST "/library/playlists/move")]
async fn reorder(cx: &Cx, Form(form): Form<MoveForm>) -> Result<SeeOther> {
    let id = form.id.clone();
    let done = shift(cx, form).await?;
    Ok(opened(
        &id,
        if done {
            Notice::PlaylistSaved
        } else {
            Notice::Invalid
        },
    ))
}

#[route(POST "/library/playlists/shift")]
async fn shifted(cx: &Cx, Form(form): Form<MoveForm>) -> Result<Response> {
    Ok(if shift(cx, form).await? {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::CONFLICT
    }
    .into_response())
}

#[derive(Deserialize)]
struct DeleteForm {
    id: String,
}

#[route(POST "/library/playlists/delete")]
async fn delete(cx: &Cx, Form(form): Form<DeleteForm>) -> Result<SeeOther> {
    let me = require_role(cx, |u| u.playlist || u.admin).await?;
    let done = state(cx)
        .db
        .run(move |c| {
            if store::owned(c, &form.id, &me).is_err() {
                return Ok(false);
            }
            c.execute("DELETE FROM playlists WHERE id = ?1", [&form.id])?;
            Ok(true)
        })
        .await
        .map_err(fail)?;
    Ok(listed(if done {
        Notice::PlaylistDeleted
    } else {
        Notice::Invalid
    }))
}

pub fn routes(b: RouterBuilder) -> RouterBuilder {
    b.route(create)
        .route(add)
        .route(reorder)
        .route(shifted)
        .route(rename)
        .route(remove)
        .route(delete)
}
