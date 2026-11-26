use crate::components::ui::nav::anchor;
use crate::server::utils::format::escape;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

pub fn artist_href(id: &str) -> String {
    format!("/?view=artists&artist={}", escape(id))
}

pub fn album_href(id: &str) -> String {
    format!("/?view=songs&status=all&album={}", escape(id))
}

#[component]
pub async fn artist_link(cx: &Cx, id: &str, name: &str, #[default] off: bool) -> Result<impl View> {
    let href = artist_href(id);
    Ok(view! {
        if off || id.is_empty() {
            (name)
        } else {
            anchor(href: href, class: "credit", label: name, (name))
        }
    })
}

#[component]
pub async fn album_link(cx: &Cx, id: &str, title: &str, #[default] off: bool) -> Result<impl View> {
    let href = album_href(id);
    Ok(view! {
        if off || id.is_empty() {
            (title)
        } else {
            anchor(href: href, class: "credit", label: title, (title))
        }
    })
}

#[component]
pub async fn credits(
    cx: &Cx,
    list: &[(String, String)],
    #[default] off: bool,
) -> Result<impl View> {
    Ok(view! {
        for (i, pair) in list.iter().enumerate() {
            if i > 0 { ", " }
            artist_link(id: pair.0.as_str(), name: pair.1.as_str(), off: off)
        }
    })
}
