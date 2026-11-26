use crate::components::ui::nav::Page;
use crate::server::api::server::libraries::delete_post::DeleteForm;
use crate::server::utils::db::users::User;
use crate::server::utils::notice::{Notice, back};
use crate::server::utils::session::{fail, require_role, state};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[route(POST "/server/libraries/missing")]
pub async fn remove_missing(cx: &Cx, Form(form): Form<DeleteForm>) -> Result<SeeOther> {
    require_role(cx, User::can_delete).await?;
    let scanner = state(cx).scanner.clone();
    let removed = tokio::task::spawn_blocking(move || scanner.remove_missing(form.id))
        .await
        .map_err(|e| {
            fail(crate::server::utils::error::ApiError::generic(
                e.to_string(),
            ))
        })?
        .map_err(fail)?;
    Ok(back(
        Page::Server,
        if removed.is_some() {
            Notice::MissingRemoved
        } else {
            Notice::Invalid
        },
    ))
}
