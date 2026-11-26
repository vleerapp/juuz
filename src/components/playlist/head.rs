use crate::components::ui::icon::PLAY_ICON;
use crate::components::ui::nav::jump;
use crate::server::utils::format::escape;
use crate::server::utils::playlists::Playlist;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::icon::icon;
use topcoat::view::{View, component, view};

#[component]
pub async fn playlist_head(cx: &Cx, list: &Playlist, play: bool, edit: bool) -> Result<impl View> {
    let big = format!("/art?id={}&size=480", escape(&list.id));
    let editable = edit && list.mine;
    Ok(view! {
        <div class="album-head">
            if play {
                <button class="thumb big" data-play-list=(list.id.as_str()) aria-label="play playlist">
                    if list.cover {
                        <img class="cover" src=(big.as_str()) alt="">
                    } else {
                        <span class="cover"></span>
                    }
                    <span class="glyph">icon(data: PLAY_ICON)</span>
                </button>
            } else {
                <span class="thumb big">
                    if list.cover {
                        <img class="cover" src=(big.as_str()) alt="">
                    } else {
                        <span class="cover"></span>
                    }
                </span>
            }
            <div class="text">
                if editable {
                    <form class="rename" method="post" action="/library/playlists/rename" data-rename="">
                        <input type="hidden" name="id" value=(list.id.as_str())>
                        <strong class="title name" tabindex="0" role="button" aria-label="rename playlist" data-rename-open="">(list.name.as_str())</strong>
                        <span class="edit">
                            <input type="text" name="name" value=(list.name.as_str()) maxlength="100" autocomplete="off" required=(true)>
                            <button>"rename"</button>
                        </span>
                    </form>
                } else {
                    <strong class="title">(list.name.as_str())</strong>
                }
                <span class="muted">(list.detail.as_str())</span>
                if !list.comment.is_empty() {
                    <span class="muted">(list.comment.as_str())</span>
                }
                <nav class="acts">
                    if play { <button data-playall="">"play all"</button> }
                    if editable {
                        jump(href: format!("/?view=songs&add={}", escape(&list.id)), label: "add songs")
                        <form method="post" action="/library/playlists/delete" data-confirm="">
                            <input type="hidden" name="id" value=(list.id.as_str())>
                            <button class="ghost danger">"delete"</button>
                        </form>
                    }
                </nav>
            </div>
        </div>
    })
}
