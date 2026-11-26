use crate::components::credits::credits;
use crate::components::ui::icon::PLAY_ICON;
use crate::components::ui::nav::jump;
use crate::pages::library::index::Album;
use crate::server::utils::format::escape;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::icon::icon;
use topcoat::view::{View, component, view};

#[component]
pub async fn album_head(
    cx: &Cx,
    album: &Album,
    play: bool,
    #[default] catalogue: bool,
) -> Result<impl View> {
    let big = format!("/art?id={}&size=480", escape(&album.id));
    let target = format!("/library/candidates/album?id={}", escape(&album.id));
    Ok(view! {
        <div class="album-head">
            if play {
                <button class="thumb big" data-playall="" aria-label="play album">
                    if album.cover {
                        <img class="cover" src=(big.as_str()) alt="">
                    } else {
                        <span class="cover"></span>
                    }
                    <span class="glyph">icon(data: PLAY_ICON)</span>
                </button>
            } else {
                <span class="thumb big">
                    if album.cover {
                        <img class="cover" src=(big.as_str()) alt="">
                    } else {
                        <span class="cover"></span>
                    }
                </span>
            }
            <div class="text">
                <strong class="title">(album.title.as_str())</strong>
                <span class="muted">credits(list: &album.artists)</span>
                <span class="muted">(album.detail.as_str())</span>
                if !album.genres.is_empty() {
                    <span class="muted">(album.genres.as_str())</span>
                }
                <nav class="acts">
                    if play { <button data-playall="">"play all"</button> }
                    if catalogue { jump(href: target.clone(), label: "match") }
                </nav>
            </div>
        </div>
    })
}
