use crate::components::credits::credits;
use crate::components::ui::icon::PLAY_ICON;
use crate::components::ui::nav::{anchor, jump};
use crate::pages::library::index::Album;
use crate::server::utils::format::escape;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::icon::icon;
use topcoat::view::{View, component, view};

#[component]
pub async fn album_tile(cx: &Cx, album: &Album, href: String, play: bool) -> Result<impl View> {
    let open = href.clone();
    let big = format!("/art?id={}&size=480", escape(&album.id));
    Ok(view! {
        <div class="album-tile">
            if play {
                <button class="thumb big" data-play-album=(album.id.as_str()) aria-label="play album">
                    if album.cover {
                        <img class="cover" src=(big.as_str()) alt="" loading="lazy">
                    } else {
                        <span class="cover"></span>
                    }
                    <span class="glyph">icon(data: PLAY_ICON)</span>
                </button>
            } else {
                anchor(href: open, class: "thumb big", label: "open album",
                    if album.cover {
                        <img class="cover" src=(big.as_str()) alt="" loading="lazy">
                    } else {
                        <span class="cover"></span>
                    }
                )
            }
            <div class="text">
                <strong>jump(href: href, label: album.title.as_str())</strong>
                <span class="muted">credits(list: &album.artists)</span>
            </div>
        </div>
    })
}
