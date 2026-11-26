use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

#[component]
pub async fn playlist_create(cx: &Cx) -> Result<impl View> {
    Ok(view! {
        <form class="create" method="post" action="/library/playlists/create">
            <label class="search">
                <input type="text" name="name" placeholder="new playlist" autocomplete="off" required=(true)>
            </label>
            <button>"create"</button>
        </form>
    })
}
