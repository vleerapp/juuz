use crate::server::api::rest::playlists::{self as store};
use crate::server::utils::db::libraries;
use crate::server::utils::notice::Notice;
use crate::server::utils::playlists::{MAX_ADD, opened};
use crate::server::utils::session::{fail, require_role, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct AddForm {
    pub id: String,
    pub songs: String,
}

#[route(POST "/library/playlists/add")]
pub async fn add(cx: &Cx, Form(form): Form<AddForm>) -> Result<SeeOther> {
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
