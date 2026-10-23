use std::time::UNIX_EPOCH;

use serde::Deserialize;
use topcoat::Result;
use topcoat::context::{Cx, app_context};
use topcoat::router::content::Form;
use topcoat::router::error::{SeeOther, see_other};
use topcoat::router::route;
use topcoat::session;

use super::nav::Page;
use super::notice::{Notice, back};
use crate::auth::verify_password_async;
use crate::db::now;
use crate::db::users::{self, User};
use crate::error::ApiError;
use crate::state::AppState;

pub fn state(cx: &Cx) -> &AppState {
    app_context::<AppState>(cx)
}

pub fn https(cx: &Cx) -> bool {
    let headers = topcoat::router::request::headers(cx);
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim);
    let forwarded = headers
        .get("forwarded")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("proto=https"));
    scheme.is_some_and(|s| s.eq_ignore_ascii_case("https")) || forwarded
}

pub fn fail(e: ApiError) -> topcoat::Error {
    e.into()
}

pub async fn current(cx: &Cx) -> Result<Option<User>> {
    let Some(hash) = session::token_hash(cx).await? else {
        return Ok(None);
    };
    let key = hash.to_vec();
    state(cx)
        .db
        .run(move |c| Ok(users::by_session(c, &key, now())?))
        .await
        .map_err(fail)
}

pub async fn require(cx: &Cx) -> Result<User> {
    match current(cx).await? {
        Some(user) => Ok(user),
        None => Err(see_other("/").into()),
    }
}

pub async fn require_admin(cx: &Cx) -> Result<User> {
    let user = require(cx).await?;
    if user.admin {
        Ok(user)
    } else {
        Err(back(Page::Stats, Notice::Forbidden).into())
    }
}

pub async fn token(cx: &Cx) -> Result<Vec<u8>> {
    Ok(session::token_hash(cx)
        .await?
        .map(|h| h.to_vec())
        .unwrap_or_default())
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

#[route(POST "/login")]
async fn login(cx: &Cx, Form(form): Form<LoginForm>) -> Result<SeeOther> {
    let st = state(cx);
    let name = form.username.trim().to_string();
    let key = crate::ratelimit::client_key(topcoat::router::request::headers(cx));
    let user = st
        .db
        .run(move |c| Ok(users::by_name(c, &name)?))
        .await
        .map_err(fail)?;
    if st.ratelimit.locked(&key) {
        return Ok(back(Page::Stats, Notice::LoginFailed));
    }
    let valid = verify_password_async(
        user.as_ref().map(|u| u.password_hash.as_str()),
        &form.password,
    )
    .await;
    let Some(user) = user.filter(|_| valid) else {
        st.ratelimit.fail(&key);
        return Ok(back(Page::Stats, Notice::LoginFailed));
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

#[route(POST "/logout")]
async fn logout(cx: &Cx) -> Result<SeeOther> {
    if let Some(hash) = session::stop(cx).await? {
        let key = hash.to_vec();
        state(cx)
            .db
            .run(move |c| Ok(users::delete_session(c, &key)?))
            .await
            .map_err(fail)?;
    }
    Ok(see_other("/"))
}

pub fn routes(b: topcoat::router::RouterBuilder) -> topcoat::router::RouterBuilder {
    b.route(login).route(logout)
}
