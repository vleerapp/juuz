use crate::components::ui::icon::PLAY_ICON;
use crate::components::ui::nav::jump;
use crate::pages::library::index::Artist;
use crate::server::utils::format::escape;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::icon::icon;
use topcoat::view::{View, component, view};

#[component]
pub async fn artist_head(
    cx: &Cx,
    artist: &Artist,
    play: bool,
    #[default] catalogue: bool,
) -> Result<impl View> {
    let big = format!("/art?id={}&size=480", escape(&artist.id));
    let target = format!("/library/candidates/artist?id={}", escape(&artist.id));
    Ok(view! {
        <div class="album-head">
            if play {
                <button class="thumb big round" data-playall="" aria-label="play artist">
                    if artist.image {
                        <img class="cover" src=(big.as_str()) alt="">
                    } else {
                        <span class="cover"></span>
                    }
                    <span class="glyph">icon(data: PLAY_ICON)</span>
                </button>
            } else {
                <span class="thumb big round">
                    if artist.image {
                        <img class="cover" src=(big.as_str()) alt="">
                    } else {
                        <span class="cover"></span>
                    }
                </span>
            }
            <div class="text">
                <strong class="title">(artist.name.as_str())</strong>
                <span class="muted">(artist.detail.as_str())</span>
                <nav class="acts">
                    if play { <button data-playall="">"play all"</button> }
                    if catalogue { jump(href: target.clone(), label: "match") }
                </nav>
            </div>
        </div>
    })
}
