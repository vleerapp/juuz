use crate::db::users::User;

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
            Page::Library => "/library",
            Page::Stats => "/",
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
            Page::Server => user.can_server() || user.can_scan() || user.can_library(),
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
