use crate::components::ui::nav::Page;
use crate::server::utils::db::users;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{fail, require_admin, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct DeleteForm {
    pub username: String,
}

#[route(POST "/users/delete")]
pub async fn delete_user(cx: &Cx, Form(form): Form<DeleteForm>) -> Result<SeeOther> {
    let me = require_admin(cx).await?;
    let outcome = state(cx)
        .db
        .run(move |c| {
            let Some(target) = users::by_name(c, &form.username)? else {
                return Ok(Notice::Invalid);
            };
            if target.id == me.id {
                return Ok(Notice::SelfProtected);
            }
            users::delete(c, &target.id)?;
            Ok(Notice::UserDeleted)
        })
        .await
        .map_err(fail)?;
    Ok(back(Page::Users, outcome))
}
