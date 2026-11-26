use crate::server::utils::db::now;
use crate::server::utils::notice::Notice;
use crate::server::utils::playlists::{clean, listed, opened};
use crate::server::utils::session::{fail, require_role, state};
use rusqlite::params;
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct CreateForm {
    pub name: String,
}

#[route(POST "/library/playlists/create")]
pub async fn create(cx: &Cx, Form(form): Form<CreateForm>) -> Result<SeeOther> {
    let me = require_role(cx, |u| u.playlist || u.admin).await?;
    let Some(name) = clean(&form.name) else {
        return Ok(listed(Notice::Invalid));
    };
    let id = crate::server::utils::ids::random();
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
