use crate::components::ui::nav::jump;
use crate::server::utils::data::Range;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{View, component, view};

#[derive(Clone)]
pub struct Filters {
    pub range: Range,
    pub everyone: bool,
    pub admin: bool,
}

pub fn view_link(range: Range, everyone: bool) -> String {
    let scope = if everyone { "&scope=all" } else { "" };
    format!("/stats?range={}{scope}", range.code())
}

#[component]
pub async fn filter_bar(cx: &Cx, filters: Filters) -> Result<impl View> {
    Ok(view! {
        <nav class="filters" aria-label="filters">
            <div class="group" role="group" aria-label="range">
                for range in Range::EACH {
                    jump(href: view_link(range, filters.everyone), label: range.label(), on: range == filters.range)
                }
            </div>
            if filters.admin {
                <div class="group" role="group" aria-label="view">
                    jump(href: view_link(filters.range, false), label: "just me", on: !filters.everyone)
                    jump(href: view_link(filters.range, true), label: "everyone", on: filters.everyone)
                </div>
            }
        </nav>
    })
}
