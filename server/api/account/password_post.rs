use crate::components::ui::nav::Page;
use crate::server::utils::auth;
use crate::server::utils::auth::verify_password_async;
use crate::server::utils::db::users;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{fail, require, state, token};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::{request, route};

#[derive(Deserialize)]
pub struct PasswordForm {
    pub current: String,
    pub new: String,
    pub confirm: String,
}

#[route(POST "/account/password")]
pub async fn password(cx: &Cx, Form(form): Form<PasswordForm>) -> Result<SeeOther> {
    let user = require(cx).await?;
    let st = state(cx);
    if form.new.is_empty() {
        return Ok(back(Page::Account, Notice::Invalid));
    }
    if form.new != form.confirm {
        return Ok(back(Page::Account, Notice::PasswordMismatch));
    }
    let key = crate::server::middleware::rate_limit::client_key(request::headers(cx));
    if st.ratelimit.locked(&key) {
        return Ok(back(Page::Account, Notice::PasswordWrong));
    }
    if !verify_password_async(Some(&user.password_hash), &form.current).await {
        st.ratelimit.fail(&key);
        return Ok(back(Page::Account, Notice::PasswordWrong));
    }
    let (hash, sealed) = auth::credentials(&st.crypto, &form.new)
        .await
        .map_err(fail)?;
    let keep = token(cx).await?;
    st.db
        .run(move |c| {
            users::set_password(c, &user.id, &hash, sealed.as_deref())?;
            users::delete_sessions_except(c, &user.id, &keep)?;
            Ok(())
        })
        .await
        .map_err(fail)?;
    Ok(back(Page::Account, Notice::PasswordChanged))
}
