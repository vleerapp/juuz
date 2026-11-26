use crate::components::ui::nav::Page;
use crate::server::utils::db::libraries;
use crate::server::utils::db::libraries::Invalid;
use crate::server::utils::db::users::User;
use crate::server::utils::libraries::allowed_path;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{fail, require_role, state};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[derive(Deserialize)]
pub struct CreateForm {
    pub name: String,
    pub path: String,
    pub writable: Option<String>,
}

#[route(POST "/server/libraries/create")]
pub async fn create_library(cx: &Cx, Form(form): Form<CreateForm>) -> Result<SeeOther> {
    let me = require_role(cx, User::can_library).await?;
    let st = state(cx);
    let writable = form.writable.is_some();
    let path = std::path::PathBuf::from(form.path.trim());
    if !allowed_path(&st.cfg, &path, me.admin) {
        return Ok(back(Page::Server, Notice::LibraryPath));
    }
    let created = st
        .db
        .run(move |c| Ok(libraries::create(c, &form.name, &path, writable)))
        .await
        .map_err(fail)?;
    let notice = match created {
        Ok(id) => {
            st.db.refresh_roots().map_err(fail)?;
            st.scanner.start_library(Some(id), false);
            Notice::LibraryAdded
        }
        Err(Invalid::Name) => Notice::Invalid,
        Err(Invalid::Path) => Notice::LibraryPath,
        Err(Invalid::Overlap) => Notice::LibraryOverlap,
        Err(Invalid::Exists) => Notice::LibraryExists,
    };
    Ok(back(Page::Server, notice))
}
