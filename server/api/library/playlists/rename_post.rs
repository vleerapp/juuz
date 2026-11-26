use crate::components::ui::nav::Page;
use crate::server::api::rest::playlists::{self as store};
use crate::server::utils::db::now;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::playlists::{clean, opened};
use crate::server::utils::session::{fail, require_role, state};
use rusqlite::params;
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct RenameForm {
    pub id: String,
    pub name: String,
}

#[route(POST "/library/playlists/rename")]
pub async fn rename(cx: &Cx, Form(form): Form<RenameForm>) -> Result<SeeOther> {
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
