use crate::components::ui::nav::jump;
use crate::server::utils::playlists::{Playlist, href};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

#[component]
pub async fn adding_note(cx: &Cx, list: &Playlist) -> Result<impl View> {
    Ok(view! {
        <div class="adding">
            <span>"adding to " <strong>(list.name.as_str())</strong></span>
            <nav class="acts">jump(href: href(&list.id), label: "cancel")</nav>
            <form method="post" action="/library/playlists/add" data-pick="">
                <input type="hidden" name="id" value=(list.id.as_str())>
                <input type="hidden" name="songs" value="">
                <button disabled=(true)>"add 0 songs"</button>
            </form>
        </div>
    })
}
