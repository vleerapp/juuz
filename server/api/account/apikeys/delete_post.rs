use crate::components::ui::nav::Page;
use crate::server::utils::db::users;
use crate::server::utils::db::users::User;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{fail, require_role, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct KeyDeleteForm {
    pub id: String,
}

#[route(POST "/account/apikeys/delete")]
pub async fn delete_key(cx: &Cx, Form(form): Form<KeyDeleteForm>) -> Result<SeeOther> {
    let user = require_role(cx, User::can_keys).await?;
    state(cx)
        .db
        .run(move |c| Ok(users::delete_api_key(c, &user.id, &form.id)?))
        .await
        .map_err(fail)?;
    Ok(back(Page::Account, Notice::KeyDeleted))
}
