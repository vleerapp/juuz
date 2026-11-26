use crate::components::ui::nav::Page;
use crate::server::utils::db::users;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{fail, require, state};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[route(POST "/account/lastfm/unlink")]
pub async fn lastfm_unlink(cx: &Cx) -> Result<SeeOther> {
    let user = require(cx).await?;
    state(cx)
        .db
        .run(move |c| Ok(users::set_lastfm(c, &user.id, None)?))
        .await
        .map_err(fail)?;
    Ok(back(Page::Account, Notice::LastfmUnlinked))
}
