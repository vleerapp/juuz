use crate::server::utils::format::escape;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

#[component]
pub async fn cover(cx: &Cx, id: &str, has: bool) -> Result<impl View> {
    let src = format!("/art?id={}&size=128", escape(id));
    Ok(view! {
        if has {
            <img class="cover" src=(src.as_str()) alt="" loading="lazy">
        } else {
            <span class="cover"></span>
        }
    })
}

#[component]
pub async fn candidate_cover(cx: &Cx, src: &str) -> Result<impl View> {
    Ok(view! {
        if src.is_empty() {
            <span class="cover"></span>
        } else {
            <img class="cover" src=(src) alt="" loading="lazy" referrerpolicy="no-referrer">
        }
    })
}
