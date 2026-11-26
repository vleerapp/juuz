use crate::server::utils::db::users;
use crate::server::utils::session::fail;
use crate::server::utils::session::state;
use topcoat::context::Cx;
use topcoat::router::error::{SeeOther, see_other};
use topcoat::router::route;
use topcoat::{Result, session};

#[route(POST "/logout")]
pub async fn logout(cx: &Cx) -> Result<SeeOther> {
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
