use crate::server::utils::catalogue::{PickForm, pick_entity};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[route(POST "/library/pick/album")]
pub async fn pick_album(cx: &Cx, Form(form): Form<PickForm>) -> Result<SeeOther> {
    pick_entity(cx, form, "album").await
}
