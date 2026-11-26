use crate::server::api::rest::playlists::{self as store};
use crate::server::utils::notice::Notice;
use crate::server::utils::playlists::opened;
use crate::server::utils::session::{fail, require_role, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct RemoveForm {
    pub id: String,
    pub song: String,
    pub index: usize,
}

#[route(POST "/library/playlists/remove")]
pub async fn remove(cx: &Cx, Form(form): Form<RemoveForm>) -> Result<SeeOther> {
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
