use crate::components::ui::nav::Page;
use crate::server::utils::db::users;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{LASTFM_COOKIE, LASTFM_PATH, fail, https, require, state};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::cookie::{Cookie, Cookies, cookies};
use topcoat::router::error::SeeOther;
use topcoat::router::{query_params, route};

#[query_params(error = bad_request)]
pub struct LastfmCallback {
    pub token: Option<String>,
    pub state: Option<String>,
}

#[route(GET "/account/lastfm/callback")]
pub async fn lastfm_callback(cx: &Cx) -> Result<SeeOther> {
    let user = require(cx).await?;
    let st = state(cx);
    let query = query_params::<LastfmCallback>(cx)?;
    let jar = cookies(cx);
    let expected = jar.get(LASTFM_COOKIE).map(|c| c.value().to_string());
    jar.remove(
        Cookie::build((LASTFM_COOKIE, ""))
            .path(LASTFM_PATH)
            .secure(https(cx))
            .build(),
    );
    let (Some(client), Some(expected), Some(given), Some(token)) = (
        st.lastfm.clone(),
        expected,
        query.state.clone(),
        query.token.clone(),
    ) else {
        return Ok(back(Page::Account, Notice::LastfmFailed));
    };
    let differs = expected
        .bytes()
        .zip(given.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    if expected.len() != given.len() || differs != 0 {
        return Ok(back(Page::Account, Notice::LastfmFailed));
    }
    let session = tokio::task::spawn_blocking(move || client.session(&token))
        .await
        .ok()
        .and_then(Result::ok);
    let Some((key, name)) = session else {
        return Ok(back(Page::Account, Notice::LastfmFailed));
    };
    let sealed = st.crypto.seal(key.as_bytes());
    st.db
        .run(move |c| Ok(users::set_lastfm(c, &user.id, Some((&sealed, &name)))?))
        .await
        .map_err(fail)?;
    Ok(back(Page::Account, Notice::LastfmLinked))
}
