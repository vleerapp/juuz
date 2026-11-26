use crate::components::credits::{album_link, artist_link, credits};
use crate::components::ui::panel::panel;
use crate::server::utils::data::{Ranked, Recent};
use crate::server::utils::format::{ago, duration};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

#[component]
pub async fn tile(cx: &Cx, label: &str, value: String) -> Result<impl View> {
    Ok(view! {
        <div class="tile"><dt>(label)</dt><dd>(value)</dd></div>
    })
}

#[component]
pub async fn ranked_list(
    cx: &Cx,
    title: &str,
    kind: &str,
    items: &[Ranked],
    links: bool,
) -> Result<impl View> {
    Ok(view! {
        panel(
            title: title,
            if items.is_empty() {
                <p class="muted">"nothing yet"</p>
            } else {
                <ol class="ranked">
                    for item in items.iter() {
                        <li>
                            <span class="title">
                                if kind == "album" {
                                    album_link(id: item.id.as_str(), title: item.name.as_str(), off: !links)
                                } else if kind == "artist" {
                                    artist_link(id: item.id.as_str(), name: item.name.as_str(), off: !links)
                                } else {
                                    (item.name.as_str())
                                }
                                if !item.sub.is_empty() { <small>credits(list: &item.sub, off: !links)</small> }
                            </span>
                            <span class="detail">(duration(item.listened_ms))</span>
                            <span class="count">(item.plays)</span>
                        </li>
                    }
                </ol>
            }
        )
    })
}

#[component]
pub async fn recent_list(
    cx: &Cx,
    items: &[Recent],
    everyone: bool,
    tz: i64,
    links: bool,
) -> Result<impl View> {
    Ok(view! {
        panel(
            title: "recent plays",
            if items.is_empty() {
                <p class="muted">"nothing yet"</p>
            } else {
                <ul>
                    for item in items.iter() {
                        <li>
                            <span class="title">(item.title.as_str()) <small>credits(list: &item.artists, off: !links)</small></span>
                            <span class="detail">
                                if everyone { (item.user.as_str()) " · " }
                                (ago(item.played_at, tz))
                            </span>
                        </li>
                    }
                </ul>
            }
        )
    })
}
