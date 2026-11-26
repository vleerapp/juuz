use crate::components::credits::credits;
use crate::components::ui::icon::PLAY_ICON;
use crate::components::ui::nav::jump;
use crate::pages::library::index::{Song, short_id};
use crate::server::utils::format::escape;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::icon::icon;
use topcoat::view::{View, component, view};

#[component]
pub async fn track_row(
    cx: &Cx,
    song: &Song,
    play: bool,
    index: usize,
    catalogue: bool,
) -> Result<impl View> {
    let number = if song.number.is_empty() {
        index.to_string()
    } else {
        song.number.clone()
    };
    let cover_flag = if song.cover { "1" } else { "" };
    let target = format!("/library/candidates?id={}", escape(&song.id));
    let status = match &song.omm_id {
        Some(id) => short_id(id).to_string(),
        None => "none".to_string(),
    };
    Ok(view! {
        <div class="row">
            <span class="c-num">
                if play {
                    <button class="number" data-play=(song.id.as_str()) data-title=(song.title.as_str()) data-artist=(song.artist.as_str()) data-album=(song.album.as_str()) data-art=(song.album_id.as_str()) data-cover=(cover_flag) data-gain=(song.gain.as_str()) data-album-id=(song.album_id.as_str()) data-artist-id=(song.artist_id.as_str()) data-starred=(if song.starred { "1" } else { "" }) aria-label="play">
                        <span class="n">(number.as_str())</span>
                        <span class="glyph">icon(data: PLAY_ICON)</span>
                    </button>
                } else {
                    <span class="muted">(number.as_str())</span>
                }
            </span>
            <span class="main">
                <strong>(song.title.as_str())</strong>
                <span class="muted">credits(list: &song.artists)</span>
            </span>
            if catalogue {
                <span class="c-meta wide-only"><nav class="choose">jump(href: target, label: "match")</nav><span class="muted">(status.as_str())</span></span>
            }
            <span class="c-time muted">(song.length.as_str())</span>
        </div>
    })
}
