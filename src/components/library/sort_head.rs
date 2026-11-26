use crate::components::ui::icon::{ARROW_DOWN_ICON, ARROW_UP_ICON, DURATION_ICON};
use crate::components::ui::nav::anchor;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::icon::icon;
use topcoat::view::{View, component, view};

#[component]
pub async fn sort_head(
    cx: &Cx,
    href: String,
    label: &str,
    active: bool,
    desc: bool,
    clock: bool,
) -> Result<impl View> {
    let class = if active { "sort active" } else { "sort" };
    Ok(view! {
        anchor(href: href, class: class, label: label,
            if clock {
                icon(data: DURATION_ICON)
            } else {
                (label)
            }
            if active {
                if desc {
                    icon(data: ARROW_DOWN_ICON)
                } else {
                    icon(data: ARROW_UP_ICON)
                }
            }
        )
    })
}
