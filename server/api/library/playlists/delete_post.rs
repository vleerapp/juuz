use crate::server::api::rest::playlists::{self as store};
use crate::server::utils::notice::Notice;
use crate::server::utils::playlists::listed;
use crate::server::utils::session::{fail, require_role, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct DeleteForm {
    pub id: String,
}

#[route(POST "/library/playlists/delete")]
pub async fn delete(cx: &Cx, Form(form): Form<DeleteForm>) -> Result<SeeOther> {
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
