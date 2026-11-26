use crate::app::logo_version;
use crate::components::banner::banner;
use crate::components::filter_bar::{Filters, filter_bar};
use crate::components::ui::nav::{Page, jump};
use crate::server::utils::db::users::User;
use crate::server::utils::notice::Notice;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::view::{Child, View, class, component, view};

#[component]
pub async fn shell(
    cx: &Cx,
    page: Option<Page>,
    name: &str,
    me: Option<User>,
    notice: Option<Notice>,
    #[default] filters: Option<Filters>,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    let authed = !name.is_empty();
    let remote = if me.as_ref().is_some_and(|u| u.jukebox || u.admin) {
        "1"
    } else {
        ""
    };
    let pages: Vec<Page> = me
        .as_ref()
        .map(|u| Page::EACH.into_iter().filter(|p| p.visible(u)).collect())
        .unwrap_or_default();
    Ok(view! {
        <div class=(class!("screen", "guest" if !authed)) data-jukebox=(remote)>
            <header>
                <h1><img src=(format!("/logo.svg?v={:x}", logo_version())) alt="juuz"></h1>
                if authed {
                    <div class="session">
                        <span>(name)</span>
                        <form method="post" action="/logout"><button class="ghost">"sign out"</button></form>
                    </div>
                }
            </header>
            if authed {
                <div class="bar">
                    <nav class="tabs" aria-label="pages">
                        for entry in pages.iter().copied() {
                            jump(href: entry.path().to_string(), label: entry.label(), key: entry.key().to_string(), on: Some(entry) == page)
                        }
                    </nav>
                    match filters {
                        Some(filters) => filter_bar(filters: filters),
                        None => "",
                    }
                </div>
            }
            banner(notice: notice)
            <main>(child)</main>
        </div>
    })
}
