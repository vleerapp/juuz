use crate::server::utils::catalogue::{PickForm, pick_entity};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[route(POST "/library/pick/artist")]
pub async fn pick_artist(cx: &Cx, Form(form): Form<PickForm>) -> Result<SeeOther> {
    pick_entity(cx, form, "artist").await
}
