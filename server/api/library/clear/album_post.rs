use crate::server::utils::catalogue::{ClearForm, clear_entity};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[route(POST "/library/clear/album")]
pub async fn clear_album(cx: &Cx, Form(form): Form<ClearForm>) -> Result<SeeOther> {
    clear_entity(cx, form, "album").await
}
