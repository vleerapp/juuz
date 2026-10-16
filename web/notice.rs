use topcoat::router::error::{SeeOther, see_other};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    LoginFailed,
    Forbidden,
    Invalid,
    Saved,
    PasswordChanged,
    PasswordWrong,
    PasswordMismatch,
    KeyDeleted,
    LastfmLinked,
    LastfmUnlinked,
    LastfmFailed,
    UserCreated,
    UserUpdated,
    UserDeleted,
    UserExists,
    SelfProtected,
    ScanStarted,
    ScanBusy,
}

impl Notice {
    const EACH: [Notice; 18] = [
        Notice::LoginFailed,
        Notice::Forbidden,
        Notice::Invalid,
        Notice::Saved,
        Notice::PasswordChanged,
        Notice::PasswordWrong,
        Notice::PasswordMismatch,
        Notice::KeyDeleted,
        Notice::LastfmLinked,
        Notice::LastfmUnlinked,
        Notice::LastfmFailed,
        Notice::UserCreated,
        Notice::UserUpdated,
        Notice::UserDeleted,
        Notice::UserExists,
        Notice::SelfProtected,
        Notice::ScanStarted,
        Notice::ScanBusy,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Notice::LoginFailed => "login-failed",
            Notice::Forbidden => "forbidden",
            Notice::Invalid => "invalid",
            Notice::Saved => "saved",
            Notice::PasswordChanged => "password-changed",
            Notice::PasswordWrong => "password-wrong",
            Notice::PasswordMismatch => "password-mismatch",
            Notice::KeyDeleted => "key-deleted",
            Notice::LastfmLinked => "lastfm-linked",
            Notice::LastfmUnlinked => "lastfm-unlinked",
            Notice::LastfmFailed => "lastfm-failed",
            Notice::UserCreated => "user-created",
            Notice::UserUpdated => "user-updated",
            Notice::UserDeleted => "user-deleted",
            Notice::UserExists => "user-exists",
            Notice::SelfProtected => "self-protected",
            Notice::ScanStarted => "scan-started",
            Notice::ScanBusy => "scan-busy",
        }
    }

    pub fn parse(code: Option<&str>) -> Option<Notice> {
        Notice::EACH.into_iter().find(|n| Some(n.code()) == code)
    }

    pub fn text(self) -> &'static str {
        match self {
            Notice::LoginFailed => "wrong username or password",
            Notice::Forbidden => "admin access required",
            Notice::Invalid => "invalid input",
            Notice::Saved => "settings saved",
            Notice::PasswordChanged => "password changed, other sessions signed out",
            Notice::PasswordWrong => "current password is wrong",
            Notice::PasswordMismatch => "new passwords do not match",
            Notice::KeyDeleted => "api key deleted",
            Notice::LastfmLinked => "last.fm linked",
            Notice::LastfmUnlinked => "last.fm unlinked",
            Notice::LastfmFailed => "last.fm request failed",
            Notice::UserCreated => "user created",
            Notice::UserUpdated => "user updated",
            Notice::UserDeleted => "user deleted",
            Notice::UserExists => "username already exists",
            Notice::SelfProtected => "you cannot demote or delete your own account",
            Notice::ScanStarted => "scan started",
            Notice::ScanBusy => "a scan is already running",
        }
    }

    pub fn is_error(self) -> bool {
        matches!(
            self,
            Notice::LoginFailed
                | Notice::Forbidden
                | Notice::Invalid
                | Notice::PasswordWrong
                | Notice::PasswordMismatch
                | Notice::LastfmFailed
                | Notice::UserExists
                | Notice::SelfProtected
                | Notice::ScanBusy
        )
    }
}

pub fn back(notice: Notice) -> SeeOther {
    see_other(format!("/?n={}", notice.code()))
}
