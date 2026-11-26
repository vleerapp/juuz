use crate::components::ui::nav::Page;
use crate::server::utils::auth;
use crate::server::utils::db::users;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{MAX_FIELD, fail, require_admin, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct CreateForm {
    pub username: String,
    pub password: String,
    pub admin: Option<String>,
}

#[route(POST "/users/create")]
pub async fn create_user(cx: &Cx, Form(form): Form<CreateForm>) -> Result<SeeOther> {
    require_admin(cx).await?;
    let st = state(cx);
    let name = form.username.trim().to_string();
    if name.is_empty() || name.len() > MAX_FIELD || form.password.is_empty() {
        return Ok(back(Page::Users, Notice::Invalid));
    }
    let (hash, sealed) = auth::credentials(&st.crypto, &form.password)
        .await
        .map_err(fail)?;
    let admin = form.admin.is_some();
    let created = st
        .db
        .run(move |c| {
            if users::by_name(c, &name)?.is_some() {
                return Ok(false);
            }
            users::create(c, &name, &hash, sealed.as_deref(), admin)?;
            Ok(true)
        })
        .await
        .map_err(fail)?;
    Ok(back(
        Page::Users,
        if created {
            Notice::UserCreated
        } else {
            Notice::UserExists
        },
    ))
}
