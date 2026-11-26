use crate::components::ui::nav::Page;
use crate::server::utils::db::users::User;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{require_role, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct ScanForm {
    pub id: i64,
    pub full: Option<String>,
}

#[route(POST "/server/libraries/scan")]
pub async fn scan_library(cx: &Cx, Form(form): Form<ScanForm>) -> Result<SeeOther> {
    require_role(cx, User::can_scan).await?;
    let started = state(cx)
        .scanner
        .start_library(Some(form.id), form.full.is_some());
    Ok(back(
        Page::Server,
        if started {
            Notice::ScanStarted
        } else {
            Notice::ScanBusy
        },
    ))
}
