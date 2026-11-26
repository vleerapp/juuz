use crate::components::ui::nav::Page;
use crate::server::utils::db::libraries;
use crate::server::utils::db::libraries::Invalid;
use crate::server::utils::db::users::User;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{fail, require_role, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct UpdateForm {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub minutes: String,
    pub writable: Option<String>,
}

#[route(POST "/server/libraries/update")]
pub async fn update_library(cx: &Cx, Form(form): Form<UpdateForm>) -> Result<SeeOther> {
    require_role(cx, User::can_library).await?;
    let minutes = match form.minutes.trim() {
        "" => 0,
        v => match v.parse::<i64>() {
            Ok(m) if (0..=525_600).contains(&m) => m,
            _ => return Ok(back(Page::Server, Notice::Invalid)),
        },
    };
    let writable = form.writable.is_some();
    let updated = state(cx)
        .db
        .run(move |c| {
            Ok(libraries::update(
                c,
                form.id,
                &form.name,
                writable,
                minutes * 60,
            ))
        })
        .await
        .map_err(fail)?;
    let notice = match updated {
        Ok(true) => Notice::LibraryUpdated,
        Ok(false) => Notice::Invalid,
        Err(Invalid::Name) => Notice::Invalid,
        Err(_) => Notice::LibraryExists,
    };
    Ok(back(Page::Server, notice))
}
