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
    pub full: Option<String>,
}

#[route(POST "/server/scan")]
pub async fn scan(cx: &Cx, Form(form): Form<ScanForm>) -> Result<SeeOther> {
    require_role(cx, User::can_scan).await?;
    let started = state(cx).scanner.start(form.full.is_some());
    Ok(back(
        Page::Server,
        if started {
            Notice::ScanStarted
        } else {
            Notice::ScanBusy
        },
    ))
}
