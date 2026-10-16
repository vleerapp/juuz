#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Stats,
    Account,
    Keys,
    Users,
    Server,
}

impl Page {
    pub const EACH: [Page; 5] = [
        Page::Stats,
        Page::Account,
        Page::Keys,
        Page::Users,
        Page::Server,
    ];

    pub fn path(self) -> &'static str {
        match self {
            Page::Stats => "/",
            Page::Account => "/account",
            Page::Keys => "/account/keys",
            Page::Users => "/admin",
            Page::Server => "/admin/server",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Page::Stats => "stats",
            Page::Account => "account",
            Page::Keys => "keys",
            Page::Users => "users",
            Page::Server => "server",
        }
    }

    pub fn admin(self) -> bool {
        matches!(self, Page::Users | Page::Server)
    }

    pub fn key(self) -> usize {
        Page::EACH
            .iter()
            .position(|p| *p == self)
            .map_or(0, |i| i + 1)
    }
}
