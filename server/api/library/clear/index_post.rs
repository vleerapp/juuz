use crate::components::ui::nav::Page;
use crate::server::utils::catalogue::ClearForm;
use crate::server::utils::db::libraries;
use crate::server::utils::db::users::User;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::omm;
use crate::server::utils::session::{fail, require_role, state};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[route(POST "/library/clear")]
pub async fn clear(cx: &Cx, Form(form): Form<ClearForm>) -> Result<SeeOther> {
    let me = require_role(cx, User::can_catalogue).await?;
    let cleared = state(cx)
        .db
        .run(move |c| {
            if !libraries::can_access_song(c, &me.id, &form.id)? {
                return Ok(false);
            }
            let path: String =
                c.query_row("SELECT path FROM songs WHERE id = ?1", [&form.id], |r| {
                    r.get(0)
                })?;
            let file =
                crate::server::utils::scanner::tags::read_track(std::path::Path::new(&path)).ok();
            omm::clear_match(c, &form.id, file.as_ref())?;
            Ok(true)
        })
        .await
        .map_err(fail)?;
    state(cx)
        .live
        .notify_topics(crate::server::utils::live::Live::LIBRARY);
    Ok(back(
        Page::Library,
        if cleared {
            Notice::CatalogueCleared
        } else {
            Notice::Invalid
        },
    ))
}
