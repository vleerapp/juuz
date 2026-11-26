use crate::components::ui::check::check;
use crate::components::ui::field::field;
use crate::components::ui::nav::Page;
use crate::components::ui::panel::panel;
use crate::composables::use_notice::notice_of;
use crate::composables::use_timezone::timezone;
use crate::layouts::shell::shell;
use crate::server::utils::data::Account;
use crate::server::utils::db::users;
use crate::server::utils::db::users::User;
use crate::server::utils::format::ago;
use crate::server::utils::notice::Notice;
use crate::server::utils::session::{fail, require, require_role, state};
use crate::server::utils::{auth as crypto, data};
use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::page;
use topcoat::view::{View, component, view};

#[derive(Clone)]
pub enum Reveal {
    None,
    Key { name: String, key: String },
}

#[derive(Clone)]
pub struct AccountBoard {
    pub me: User,
    pub account: Account,
    pub lastfm_ready: bool,
    pub reveal: Reveal,
    pub tz: i64,
}

#[page("/account")]
pub async fn account_page(cx: &Cx) -> Result<impl View> {
    let me = require(cx).await?;
    let notice = notice_of(cx)?;
    let viewer = me.clone();
    let account = state(cx)
        .db
        .run(move |c| data::account(c, &viewer))
        .await
        .map_err(fail)?;
    let board = AccountBoard {
        me,
        account,
        lastfm_ready: state(cx).lastfm.is_some(),
        reveal: Reveal::None,
        tz: timezone(cx),
    };
    Ok(view! { account_view(board: board, notice: notice) })
}

#[derive(Deserialize)]
pub struct KeyForm {
    pub name: String,
}

#[page(POST "/account/apikeys")]
pub async fn create_key(cx: &Cx, Form(form): Form<KeyForm>) -> Result<impl View> {
    let me = require_role(cx, User::can_keys).await?;
    let st = state(cx);
    let name = form.name.trim().chars().take(80).collect::<String>();
    let key = crypto::generate_api_key();
    let hash = crypto::hash_api_key(&key);
    let (uid, label) = (me.id.clone(), name.clone());
    st.db
        .run(move |c| Ok(users::create_api_key(c, &uid, &label, &hash)?))
        .await
        .map_err(fail)?;
    let viewer = me.clone();
    let account = st
        .db
        .run(move |c| data::account(c, &viewer))
        .await
        .map_err(fail)?;
    let board = AccountBoard {
        me,
        account,
        lastfm_ready: st.lastfm.is_some(),
        reveal: Reveal::Key { name, key },
        tz: timezone(cx),
    };
    Ok(view! { account_view(board: board, notice: Some(Notice::Saved)) })
}

#[component]
pub async fn account_view(
    cx: &Cx,
    board: AccountBoard,
    notice: Option<Notice>,
) -> Result<impl View> {
    let rate = board.me.max_bit_rate.to_string();
    Ok(view! {
        shell(
            page: Some(Page::Account),
            name: board.me.name.as_str(),
            me: Some(board.me.clone()),
            notice: notice,
            panel(
                title: "profile",
                <form method="post" action="/account/profile" data-auto="">
                    field(label: "username", name: "username", kind: "text", auto: "username", value: board.me.name.as_str(), required: true)
                    field(label: "max bitrate (kbps, 0 = unlimited)", name: "max_bit_rate", kind: "text", auto: "off", value: rate.as_str(), mode: "numeric")
                    check(name: "scrobbling", label: "record my plays", checked: board.me.scrobbling)
                </form>
            )
            panel(
                title: "password",
                <form method="post" action="/account/password">
                    field(label: "current", name: "current", kind: "password", auto: "current-password", required: true)
                    field(label: "new", name: "new", kind: "password", auto: "new-password", required: true)
                    field(label: "repeat", name: "confirm", kind: "password", auto: "new-password", required: true)
                    <button>"change password"</button>
                </form>
            )
            panel(
                title: "last.fm",
                                match (&board.account.lastfm, board.lastfm_ready) {
                    (Some(name), _) => <form method="post" action="/account/lastfm/unlink">
                        <span>"linked as " <strong>(name.as_str())</strong></span>
                        <button class="ghost">"unlink"</button>
                    </form>,
                    (None, true) => <form method="post" action="/account/lastfm/start">
                        <span class="muted">"not linked"</span>
                        <button>"link"</button>
                    </form>,
                    (None, false) => <p class="muted">"not configured on this server"</p>,
                }
            )
            panel(
                title: "listenbrainz, koito, maloja",
                match &board.account.listenbrainz {
                    Some(url) => <form method="post" action="/account/listenbrainz/unlink">
                        <span>"linked to " <strong>(crate::server::utils::listenbrainz::host(url))</strong></span>
                        <button class="ghost">"unlink"</button>
                    </form>,
                    None => <form method="post" action="/account/listenbrainz/link">
                        field(label: "server url", name: "url", kind: "text", auto: "off", placeholder: "https://api.listenbrainz.org")
                        field(label: "user token", name: "token", kind: "password", auto: "off", required: true)
                        <button>"link"</button>
                    </form>,
                }
            )
            if board.me.can_keys() {
            panel(
                title: "api keys",
                                wide: true,
                                match &board.reveal {
                    Reveal::Key { name, key } => <div class="reveal">
                        <p class="caption">"new key: " (name.as_str()) " (shown once, copy it now)"</p>
                        <code data-copy="" tabindex="0" class="secret">(key.as_str())</code>
                    </div>,
                    Reveal::None => "",
                }
                if board.account.keys.is_empty() {
                    <p class="muted">"no keys"</p>
                } else {
                    <ul>
                        for key in board.account.keys.iter() {
                            <li>
                                <span class="title">
                                    (if key.name.is_empty() { "unnamed" } else { key.name.as_str() })
                                    <small>(ago(key.created, board.tz))</small>
                                </span>
                                <form method="post" action="/account/apikeys/delete">
                                    <input type="hidden" name="id" value=(key.id.as_str())>
                                    <button class="ghost danger">"delete"</button>
                                </form>
                            </li>
                        }
                    </ul>
                }
                <form method="post" action="/account/apikeys">
                    <label>
                        <span class="caption">"new key label"</span>
                        <input name="name" maxlength="80" autocomplete="off">
                    </label>
                    <button>"create key"</button>
                </form>
            )
            }
        )
    })
}
