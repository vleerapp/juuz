use crate::components::ui::nav::Page;
use crate::server::utils::db::libraries;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::scanner::index;
use crate::server::utils::session::{fail, require_admin, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct DeleteForm {
    pub id: i64,
}

#[route(POST "/server/libraries/delete")]
pub async fn delete_library(cx: &Cx, Form(form): Form<DeleteForm>) -> Result<SeeOther> {
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
