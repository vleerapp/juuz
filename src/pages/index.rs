use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::error::see_other;
use topcoat::router::page;
use topcoat::view::{View, view};

use crate::components::ui::nav::Page;
use crate::composables::use_notice::notice_of;
use crate::pages::library::index::{library_board, library_view};
use crate::pages::login::login_page;
use crate::server::utils::session::current;

#[page("/")]
pub async fn home(cx: &Cx) -> Result<impl View> {
    let notice = notice_of(cx)?;
    let board = match current(cx).await? {
        Some(me) if Page::Library.visible(&me) => Some(library_board(cx, me).await?),
        Some(_) => return Err(see_other(Page::Stats.path()).into()),
        None => None,
    };
    Ok(view! {
        match board {
            Some(board) => library_view(board: board, notice: notice),
            None => login_page(notice: notice),
        }
    })
}
