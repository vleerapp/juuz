use crate::composables::use_runtime::runtime_on;
use crate::server::utils::db::users::User;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::runtime::{link_attrs, prefetch_mode};
use topcoat::view::{Attributes, Child, View, class, component, view};

#[component]
pub async fn anchor(
    cx: &Cx,
    href: String,
    class: &str,
    label: &str,
    #[default] child: Child<'_>,
) -> Result<impl View> {
    let attrs = if runtime_on(cx) {
        link_attrs(cx, href, prefetch_mode(cx))
    } else {
        let mut plain = Attributes::with_capacity(1);
        plain.insert(cx, "href", href);
        plain
    };
    Ok(view! {
        <a (attrs) class=(class) aria-label=(label)>(child)</a>
    })
}

#[component]
pub async fn jump(
    cx: &Cx,
    href: String,
    label: &str,
    #[default] key: String,
    #[default] on: bool,
) -> Result<impl View> {
    let attrs = if runtime_on(cx) {
        link_attrs(cx, href, prefetch_mode(cx))
    } else {
        let mut plain = Attributes::with_capacity(1);
        plain.insert(cx, "href", href);
        plain
    };
    Ok(view! {
        <a (attrs) data-key=(key) class=(class!("link", "current" if on))>(label)</a>
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Library,
    Stats,
    Account,
    Users,
    Server,
}

impl Page {
    pub const EACH: [Page; 5] = [
        Page::Library,
        Page::Stats,
        Page::Account,
        Page::Users,
        Page::Server,
    ];

    pub fn path(self) -> &'static str {
        match self {
            Page::Library => "/",
            Page::Stats => "/stats",
            Page::Account => "/account",
            Page::Users => "/users",
            Page::Server => "/server",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Page::Library => "library",
            Page::Stats => "stats",
            Page::Account => "account",
            Page::Users => "users",
            Page::Server => "server",
        }
    }

    pub fn visible(self, user: &User) -> bool {
        match self {
            Page::Users => user.admin,
            Page::Library => user.stream || user.admin || user.can_catalogue(),
            Page::Server => {
                user.can_server() || user.can_scan() || user.can_library() || user.can_delete()
            }
            _ => true,
        }
    }

    pub fn key(self) -> usize {
        Page::EACH
            .iter()
            .position(|p| *p == self)
            .map_or(0, |i| i + 1)
    }
}
