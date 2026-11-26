use crate::components::match_view::match_view;
use crate::composables::use_match::match_page;
use crate::composables::use_notice::notice_of;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::page;
use topcoat::view::{View, view};

#[page("/library/candidates/album")]
pub async fn candidates_album_page(cx: &Cx) -> Result<impl View> {
    let notice = notice_of(cx)?;
    let choice = match_page(cx, "album").await?;
    Ok(view! { match_view(choice: choice, notice: notice) })
}
