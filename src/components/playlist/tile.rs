use crate::components::ui::icon::PLAY_ICON;
use crate::components::ui::nav::{anchor, jump};
use crate::server::utils::format::escape;
use crate::server::utils::playlists::{Playlist, href};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::icon::icon;
use topcoat::view::{View, component, view};

#[component]
pub async fn playlist_tile(cx: &Cx, list: &Playlist, play: bool) -> Result<impl View> {
    let open = href(&list.id);
    let big = format!("/art?id={}&size=480", escape(&list.id));
    Ok(view! {
        <div class="album-tile">
            if play {
                <button class="thumb big" data-play-list=(list.id.as_str()) aria-label="play playlist">
                    if list.cover {
                        <img class="cover" src=(big.as_str()) alt="" loading="lazy">
                    } else {
                        <span class="cover"></span>
                    }
                    <span class="glyph">icon(data: PLAY_ICON)</span>
                </button>
            } else {
                anchor(href: open.clone(), class: "thumb big", label: "open playlist",
                    if list.cover {
                        <img class="cover" src=(big.as_str()) alt="" loading="lazy">
                    } else {
                        <span class="cover"></span>
                    }
                )
            }
            <div class="text">
                <strong>jump(href: open, label: list.name.as_str())</strong>
                <span class="muted">(list.detail.as_str())</span>
            </div>
        </div>
    })
}
