use crate::components::ui::nav::Page;
use crate::server::utils::auth::verify_password_async;
use crate::server::utils::db::{now, users};
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::fail;
use crate::server::utils::session::state;
use serde::Deserialize;
use std::time::UNIX_EPOCH;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::{SeeOther, see_other};
use topcoat::router::route;
use topcoat::{Result, session};

#[derive(Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
}

#[route(POST "/login")]
pub async fn login(cx: &Cx, Form(form): Form<LoginForm>) -> Result<SeeOther> {
    let st = state(cx);
    let name = form.username.trim().to_string();
    let key =
        crate::server::middleware::rate_limit::client_key(topcoat::router::request::headers(cx));
    let user = st
        .db
        .run(move |c| Ok(users::by_name(c, &name)?))
        .await
        .map_err(fail)?;
    if st.ratelimit.locked(&key) {
        return Ok(back(Page::Library, Notice::LoginFailed));
    }
    let valid = verify_password_async(
        user.as_ref().map(|u| u.password_hash.as_str()),
        &form.password,
    )
    .await;
    let Some(user) = user.filter(|_| valid) else {
        st.ratelimit.fail(&key);
        return Ok(back(Page::Library, Notice::LoginFailed));
    };
    let started = session::start(cx).await?;
    let expires = started
        .expires_at
        .duration_since(UNIX_EPOCH)
        .map_or(now(), |d| d.as_secs() as i64);
    let (key, uid) = (started.token_hash.to_vec(), user.id);
    st.db
        .run(move |c| Ok(users::create_session(c, &key, &uid, expires)?))
        .await
        .map_err(fail)?;
    Ok(see_other("/"))
}
