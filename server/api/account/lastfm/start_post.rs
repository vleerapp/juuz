use crate::components::ui::nav::Page;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{LASTFM_COOKIE, LASTFM_PATH, https, origin, require, state};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::cookie::{Cookie, Cookies, SameSite, cookies};
use topcoat::router::error::{SeeOther, see_other};
use topcoat::router::route;

#[route(POST "/account/lastfm/start")]
pub async fn lastfm_start(cx: &Cx) -> Result<SeeOther> {
    require(cx).await?;
    let (Some(client), Some(base)) = (state(cx).lastfm.clone(), origin(cx)) else {
        return Ok(back(Page::Account, Notice::LastfmFailed));
    };
    let nonce = hex::encode(rand::random::<[u8; 16]>());
    cookies(cx).add(
        Cookie::build((LASTFM_COOKIE, nonce.clone()))
            .path(LASTFM_PATH)
            .http_only(true)
            .secure(https(cx))
            .same_site(SameSite::Lax)
            .max_age(topcoat::cookie::time::Duration::minutes(10))
            .build(),
    );
    let callback = format!("{base}{LASTFM_PATH}/callback?state={nonce}");
    Ok(see_other(client.web_auth_link(&callback)))
}
