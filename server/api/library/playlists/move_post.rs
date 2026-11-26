use crate::server::utils::notice::Notice;
use crate::server::utils::playlists::{MoveForm, opened, shift};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::error::SeeOther;
use topcoat::router::route;

#[route(POST "/library/playlists/move")]
pub async fn reorder(cx: &Cx, Form(form): Form<MoveForm>) -> Result<SeeOther> {
    let id = form.id.clone();
    let done = shift(cx, form).await?;
    Ok(opened(
        &id,
        if done {
            Notice::PlaylistSaved
        } else {
            Notice::Invalid
        },
    ))
}
