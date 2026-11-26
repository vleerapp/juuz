use crate::components::ui::nav::Page;
use crate::server::utils::db::users;
use crate::server::utils::db::users::RoleUpdate;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{MAX_BIT_RATE, MAX_FIELD, fail, require, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct ProfileForm {
    pub username: String,
    pub max_bit_rate: String,
    pub scrobbling: Option<String>,
}

#[route(POST "/account/profile")]
pub async fn profile(cx: &Cx, Form(form): Form<ProfileForm>) -> Result<SeeOther> {
    let user = require(cx).await?;
    let name = form.username.trim().to_string();
    let rate = match form.max_bit_rate.trim() {
        "" => 0,
        raw => match raw.parse::<i64>() {
            Ok(v) if (0..=MAX_BIT_RATE).contains(&v) => v,
            _ => return Ok(back(Page::Account, Notice::Invalid)),
        },
    };
    if name.is_empty() || name.len() > MAX_FIELD {
        return Ok(back(Page::Account, Notice::Invalid));
    }
    let update = RoleUpdate {
        max_bit_rate: Some(rate),
        scrobbling: Some(form.scrobbling.is_some()),
        ..RoleUpdate::default()
    };
    let renamed = state(cx)
        .db
        .run(move |c| {
            if !users::rename(c, &user.id, &name)? {
                return Ok(false);
            }
            users::update_roles(c, &user.id, &update)?;
            Ok(true)
        })
        .await
        .map_err(fail)?;
    Ok(back(
        Page::Account,
        if renamed {
            Notice::Saved
        } else {
            Notice::UserExists
        },
    ))
}
