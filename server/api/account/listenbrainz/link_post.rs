use crate::components::ui::nav::Page;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{MAX_FIELD, require, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct ListenbrainzForm {
    pub url: String,
    pub token: String,
}

#[route(POST "/account/listenbrainz/link")]
pub async fn listenbrainz_link(cx: &Cx, Form(form): Form<ListenbrainzForm>) -> Result<SeeOther> {
    let user = require(cx).await?;
    let token = form.token.trim();
    let Some(url) = crate::server::utils::listenbrainz::normalize_url(&form.url) else {
        return Ok(back(Page::Account, Notice::ListenbrainzFailed));
    };
    if token.is_empty() || token.len() > MAX_FIELD {
        return Ok(back(Page::Account, Notice::ListenbrainzFailed));
    }
    let linked =
        crate::server::api::rest::listenbrainz::link_account(state(cx), &user.id, &url, token)
            .await;
    Ok(back(
        Page::Account,
        if linked.is_ok() {
            Notice::ListenbrainzLinked
        } else {
            Notice::ListenbrainzFailed
        },
    ))
}
