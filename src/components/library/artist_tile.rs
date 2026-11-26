use crate::components::credits::artist_href;
use crate::components::ui::nav::{anchor, jump};
use crate::pages::library::index::Artist;
use crate::server::utils::format::escape;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

#[component]
pub async fn artist_tile(cx: &Cx, artist: &Artist) -> Result<impl View> {
    let href = artist_href(&artist.id);
    let big = format!("/art?id={}&size=480", escape(&artist.id));
    Ok(view! {
        <div class="album-tile">
            anchor(href: href.clone(), class: "thumb big round", label: "open artist",
                if artist.image {
                    <img class="cover" src=(big.as_str()) alt="" loading="lazy">
                } else {
                    <span class="cover"></span>
                }
            )
            <div class="text">
                <strong>jump(href: href, label: artist.name.as_str())</strong>
                <span class="muted">(artist.detail.as_str())</span>
            </div>
        </div>
    })
}
