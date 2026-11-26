use crate::components::ui::nav::Page;
use crate::server::utils::db::users::User;
use crate::server::utils::db::{now, users};
use crate::server::utils::error::ApiError;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::state::AppState;
use topcoat::context::{Cx, context};
use topcoat::router::error::see_other;
use topcoat::router::request;
use topcoat::{Result, session};

pub const MAX_BIT_RATE: i64 = 100_000;

pub const MAX_FIELD: usize = 254;

pub const LASTFM_COOKIE: &str = "juuz_lastfm";

pub const LASTFM_PATH: &str = "/account/lastfm";

pub fn origin(cx: &Cx) -> Option<String> {
    Some(crate::server::utils::util::public_base(
        state(cx).cfg.public_url.as_deref(),
        request::headers(cx),
    ))
}

pub fn state(cx: &Cx) -> &AppState {
    context::<AppState>(cx)
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
    require_role(cx, |u| u.admin).await
}

pub async fn require_role(cx: &Cx, allowed: fn(&User) -> bool) -> Result<User> {
    let user = require(cx).await?;
    if allowed(&user) {
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
