use topcoat::router::error::{SeeOther, see_other};

use super::nav::Page;

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
    JukeboxScanned,
    JukeboxFailed,
    LibraryAdded,
    LibraryUpdated,
    LibraryRemoved,
    LibraryPath,
    LibraryOverlap,
    LibraryExists,
    AccessSaved,
    CatalogueMatched,
    CatalogueCleared,
    CatalogueOff,
}

impl Notice {
    const EACH: [Notice; 30] = [
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
        Notice::JukeboxScanned,
        Notice::JukeboxFailed,
        Notice::LibraryAdded,
        Notice::LibraryUpdated,
        Notice::LibraryRemoved,
        Notice::LibraryPath,
        Notice::LibraryOverlap,
        Notice::LibraryExists,
        Notice::AccessSaved,
        Notice::CatalogueMatched,
        Notice::CatalogueCleared,
        Notice::CatalogueOff,
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
            Notice::JukeboxScanned => "jukebox-scanned",
            Notice::JukeboxFailed => "jukebox-failed",
            Notice::LibraryAdded => "library-added",
            Notice::LibraryUpdated => "library-updated",
            Notice::LibraryRemoved => "library-removed",
            Notice::LibraryPath => "library-path",
            Notice::LibraryOverlap => "library-overlap",
            Notice::LibraryExists => "library-exists",
            Notice::AccessSaved => "access-saved",
            Notice::CatalogueMatched => "catalogue-matched",
            Notice::CatalogueCleared => "catalogue-cleared",
            Notice::CatalogueOff => "catalogue-off",
        }
    }

    pub fn parse(code: Option<&str>) -> Option<Notice> {
        Notice::EACH.into_iter().find(|n| Some(n.code()) == code)
    }

    pub fn text(self) -> &'static str {
        match self {
            Notice::LoginFailed => "wrong username or password",
            Notice::Forbidden => "you do not have access to that",
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
            Notice::JukeboxScanned => "renderer scan finished",
            Notice::JukeboxFailed => "could not use that renderer",
            Notice::LibraryAdded => "library added, scanning it now",
            Notice::LibraryUpdated => "library updated",
            Notice::LibraryRemoved => "library removed",
            Notice::LibraryPath => "that path is not a readable directory",
            Notice::LibraryOverlap => "that path overlaps another library",
            Notice::LibraryExists => "a library with that name already exists",
            Notice::AccessSaved => "library access saved",
            Notice::CatalogueMatched => "metadata match saved",
            Notice::CatalogueCleared => "metadata match cleared",
            Notice::CatalogueOff => "metadata lookup is disabled on this server",
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
                | Notice::JukeboxFailed
                | Notice::LibraryPath
                | Notice::LibraryOverlap
                | Notice::LibraryExists
                | Notice::CatalogueOff
        )
    }
}

pub fn back(page: Page, notice: Notice) -> SeeOther {
    see_other(format!("{}?n={}", page.path(), notice.code()))
}
