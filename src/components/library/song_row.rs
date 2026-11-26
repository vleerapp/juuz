use crate::components::cover::cover;
use crate::components::credits::{album_link, credits};
use crate::components::ui::icon::PLAY_ICON;
use crate::components::ui::nav::jump;
use crate::pages::library::index::{Song, short_id};
use crate::server::utils::format::escape;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::icon::icon;
use topcoat::view::{View, component, view};

#[component]
pub async fn song_row(
    cx: &Cx,
    song: &Song,
    play: bool,
    catalogue: bool,
    #[default] list: &str,
    #[default] slot: usize,
    #[default] adding: &str,
) -> Result<impl View> {
    let index = slot.to_string();
    let place = if list.is_empty() {
        String::new()
    } else {
        index.clone()
    };
    let target = format!("/library/candidates?id={}", escape(&song.id));
    let status = match &song.omm_id {
        Some(id) => short_id(id).to_string(),
        None => "none".to_string(),
    };
    let cover_flag = if song.cover { "1" } else { "" };
    Ok(view! {
        <div class="row" data-slot=(place.as_str()) data-song=(song.id.as_str())>
            if !adding.is_empty() {
                <span class="c-pick">
                    <button type="button" class="pick" data-pick=(song.id.as_str()) aria-pressed="false" aria-label="select"><span class="n"></span></button>
                </span>
            }
            <span class="c-cover">
                if play {
                    <button class="thumb" data-play=(song.id.as_str()) data-title=(song.title.as_str()) data-artist=(song.artist.as_str()) data-album=(song.album.as_str()) data-art=(song.album_id.as_str()) data-cover=(cover_flag) data-gain=(song.gain.as_str()) aria-label="play">
                        cover(id: song.album_id.as_str(), has: song.cover)
                        <span class="glyph">icon(data: PLAY_ICON)</span>
                    </button>
                } else {
                    <span class="thumb">cover(id: song.album_id.as_str(), has: song.cover)</span>
                }
            </span>
            <span class="main">
                <strong>(song.title.as_str())</strong>
                <span class="muted">credits(list: &song.artists) <span class="inline-album">" - " album_link(id: song.album_id.as_str(), title: song.album.as_str())</span></span>
            </span>
            <span class="c-album wide-only muted">album_link(id: song.album_id.as_str(), title: song.album.as_str())</span>
            <span class="c-year wide-only muted">(song.year.as_str())</span>
            <span class="c-format wide-only muted">(song.format.as_str())</span>
            if catalogue {
                <span class="c-meta wide-only"><nav class="choose">jump(href: target, label: "match")</nav><span class="muted">(status.as_str())</span></span>
            }
            <span class="c-time muted">(song.length.as_str())</span>
            if !list.is_empty() {
                <span class="c-act">
                    <form method="post" action="/library/playlists/remove">
                        <input type="hidden" name="id" value=(list)>
                        <input type="hidden" name="song" value=(song.id.as_str())>
                        <input type="hidden" name="index" value=(index.as_str())>
                        <button class="ghost danger" aria-label="remove from playlist">"drop"</button>
                    </form>
                </span>
            }
        </div>
    })
}
