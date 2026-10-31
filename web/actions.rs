use serde::Deserialize;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::cookie::{Cookie, Cookies, SameSite, cookies};
use topcoat::router::RouterBuilder;
use topcoat::router::content::Form;
use topcoat::router::error::{SeeOther, see_other};
use topcoat::router::{query_params, request, route};

use super::auth::{fail, https, require, require_admin, require_role, state, token};
use super::nav::Page;
use super::notice::{Notice, back};
use crate::auth::{self, verify_password_async};
use crate::db::users::{self, RoleUpdate, User};

const MAX_BIT_RATE: i64 = 100_000;
const MAX_FIELD: usize = 254;

#[derive(Deserialize)]
struct ProfileForm {
    username: String,
    max_bit_rate: String,
    scrobbling: Option<String>,
}

#[route(POST "/account/profile")]
async fn profile(cx: &Cx, Form(form): Form<ProfileForm>) -> Result<SeeOther> {
    let user = require(cx).await?;
    let name = form.username.trim().to_string();
    let rate = match form.max_bit_rate.trim() {
        "" => 0,
        raw => match raw.parse::<i64>() {
            Ok(v) if (0..=MAX_BIT_RATE).contains(&v) => v,
            _ => return Ok(back(Page::Account, Notice::Invalid)),
        },
    };
    if name.is_empty() || name.len() > MAX_FIELD {
        return Ok(back(Page::Account, Notice::Invalid));
    }
    let update = RoleUpdate {
        max_bit_rate: Some(rate),
        scrobbling: Some(form.scrobbling.is_some()),
        ..RoleUpdate::default()
    };
    let renamed = state(cx)
        .db
        .run(move |c| {
            if !users::rename(c, &user.id, &name)? {
                return Ok(false);
            }
            users::update_roles(c, &user.id, &update)?;
            Ok(true)
        })
        .await
        .map_err(fail)?;
    Ok(back(
        Page::Account,
        if renamed {
            Notice::Saved
        } else {
            Notice::UserExists
        },
    ))
}

#[derive(Deserialize)]
struct PasswordForm {
    current: String,
    new: String,
    confirm: String,
}

#[route(POST "/account/password")]
async fn password(cx: &Cx, Form(form): Form<PasswordForm>) -> Result<SeeOther> {
    let user = require(cx).await?;
    let st = state(cx);
    if form.new.is_empty() {
        return Ok(back(Page::Account, Notice::Invalid));
    }
    if form.new != form.confirm {
        return Ok(back(Page::Account, Notice::PasswordMismatch));
    }
    let key = crate::ratelimit::client_key(request::headers(cx));
    if st.ratelimit.locked(&key) {
        return Ok(back(Page::Account, Notice::PasswordWrong));
    }
    if !verify_password_async(Some(&user.password_hash), &form.current).await {
        st.ratelimit.fail(&key);
        return Ok(back(Page::Account, Notice::PasswordWrong));
    }
    let (hash, sealed) = auth::credentials(&st.crypto, &form.new)
        .await
        .map_err(fail)?;
    let keep = token(cx).await?;
    st.db
        .run(move |c| {
            users::set_password(c, &user.id, &hash, Some(&sealed))?;
            users::delete_sessions_except(c, &user.id, &keep)?;
            Ok(())
        })
        .await
        .map_err(fail)?;
    Ok(back(Page::Account, Notice::PasswordChanged))
}

#[derive(Deserialize)]
struct KeyDeleteForm {
    id: String,
}

#[route(POST "/account/apikeys/delete")]
async fn delete_key(cx: &Cx, Form(form): Form<KeyDeleteForm>) -> Result<SeeOther> {
    let user = require_role(cx, User::can_keys).await?;
    state(cx)
        .db
        .run(move |c| Ok(users::delete_api_key(c, &user.id, &form.id)?))
        .await
        .map_err(fail)?;
    Ok(back(Page::Account, Notice::KeyDeleted))
}

const LASTFM_COOKIE: &str = "juuz_lastfm";
const LASTFM_PATH: &str = "/account/lastfm";

fn origin(cx: &Cx) -> Option<String> {
    Some(crate::util::public_base(
        state(cx).cfg.public_url.as_deref(),
        request::headers(cx),
    ))
}

#[route(POST "/account/lastfm/start")]
async fn lastfm_start(cx: &Cx) -> Result<SeeOther> {
    require(cx).await?;
    let (Some(client), Some(base)) = (state(cx).lastfm.clone(), origin(cx)) else {
        return Ok(back(Page::Account, Notice::LastfmFailed));
    };
    let nonce = hex::encode(rand::random::<[u8; 16]>());
    cookies(cx).add(
        Cookie::build((LASTFM_COOKIE, nonce.clone()))
            .path(LASTFM_PATH)
            .http_only(true)
            .secure(https(cx))
            .same_site(SameSite::Lax)
            .max_age(topcoat::cookie::time::Duration::minutes(10))
            .build(),
    );
    let callback = format!("{base}{LASTFM_PATH}/callback?state={nonce}");
    Ok(see_other(client.web_auth_link(&callback)))
}

#[query_params(error = bad_request)]
struct LastfmCallback {
    token: Option<String>,
    state: Option<String>,
}

#[route(GET "/account/lastfm/callback")]
async fn lastfm_callback(cx: &Cx) -> Result<SeeOther> {
    let user = require(cx).await?;
    let st = state(cx);
    let query = query_params::<LastfmCallback>(cx)?;
    let jar = cookies(cx);
    let expected = jar.get(LASTFM_COOKIE).map(|c| c.value().to_string());
    jar.remove(
        Cookie::build((LASTFM_COOKIE, ""))
            .path(LASTFM_PATH)
            .secure(https(cx))
            .build(),
    );
    let (Some(client), Some(expected), Some(given), Some(token)) = (
        st.lastfm.clone(),
        expected,
        query.state.clone(),
        query.token.clone(),
    ) else {
        return Ok(back(Page::Account, Notice::LastfmFailed));
    };
    let differs = expected
        .bytes()
        .zip(given.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    if expected.len() != given.len() || differs != 0 {
        return Ok(back(Page::Account, Notice::LastfmFailed));
    }
    let session = tokio::task::spawn_blocking(move || client.session(&token))
        .await
        .ok()
        .and_then(Result::ok);
    let Some((key, name)) = session else {
        return Ok(back(Page::Account, Notice::LastfmFailed));
    };
    let sealed = st.crypto.seal(key.as_bytes());
    st.db
        .run(move |c| Ok(users::set_lastfm(c, &user.id, Some((&sealed, &name)))?))
        .await
        .map_err(fail)?;
    Ok(back(Page::Account, Notice::LastfmLinked))
}

#[route(POST "/account/lastfm/unlink")]
async fn lastfm_unlink(cx: &Cx) -> Result<SeeOther> {
    let user = require(cx).await?;
    state(cx)
        .db
        .run(move |c| Ok(users::set_lastfm(c, &user.id, None)?))
        .await
        .map_err(fail)?;
    Ok(back(Page::Account, Notice::LastfmUnlinked))
}

#[derive(Deserialize)]
struct CreateForm {
    username: String,
    password: String,
    admin: Option<String>,
}

#[route(POST "/users/create")]
async fn create_user(cx: &Cx, Form(form): Form<CreateForm>) -> Result<SeeOther> {
    require_admin(cx).await?;
    let st = state(cx);
    let name = form.username.trim().to_string();
    if name.is_empty() || name.len() > MAX_FIELD || form.password.is_empty() {
        return Ok(back(Page::Users, Notice::Invalid));
    }
    let (hash, sealed) = auth::credentials(&st.crypto, &form.password)
        .await
        .map_err(fail)?;
    let admin = form.admin.is_some();
    let created = st
        .db
        .run(move |c| {
            if users::by_name(c, &name)?.is_some() {
                return Ok(false);
            }
            users::create(c, &name, &hash, Some(&sealed), admin)?;
            Ok(true)
        })
        .await
        .map_err(fail)?;
    Ok(back(
        Page::Users,
        if created {
            Notice::UserCreated
        } else {
            Notice::UserExists
        },
    ))
}

#[derive(Deserialize)]
struct UpdateForm {
    username: String,
    #[serde(default)]
    password: String,
    admin: Option<String>,
    settings: Option<String>,
    stream: Option<String>,
    download: Option<String>,
    upload: Option<String>,
    playlist: Option<String>,
    cover_art: Option<String>,
    comment: Option<String>,
    podcast: Option<String>,
    share: Option<String>,
    jukebox: Option<String>,
    keys: Option<String>,
    scan: Option<String>,
    server: Option<String>,
    radio: Option<String>,
    delete: Option<String>,
    library: Option<String>,
    catalogue: Option<String>,
}

#[route(POST "/users/update")]
async fn update_user(cx: &Cx, Form(form): Form<UpdateForm>) -> Result<SeeOther> {
    let me = require_admin(cx).await?;
    let st = state(cx);
    let roles = RoleUpdate {
        admin: Some(form.admin.is_some()),
        settings: Some(form.settings.is_some()),
        stream: Some(form.stream.is_some()),
        download: Some(form.download.is_some()),
        upload: Some(form.upload.is_some()),
        playlist: Some(form.playlist.is_some()),
        cover_art: Some(form.cover_art.is_some()),
        comment: Some(form.comment.is_some()),
        podcast: Some(form.podcast.is_some()),
        share: Some(form.share.is_some()),
        jukebox: Some(form.jukebox.is_some()),
        keys: Some(form.keys.is_some()),
        scan: Some(form.scan.is_some()),
        server: Some(form.server.is_some()),
        radio: Some(form.radio.is_some()),
        delete: Some(form.delete.is_some()),
        library: Some(form.library.is_some()),
        catalogue: Some(form.catalogue.is_some()),
        ..RoleUpdate::default()
    };
    let credentials = if form.password.is_empty() {
        None
    } else {
        Some(
            auth::credentials(&st.crypto, &form.password)
                .await
                .map_err(fail)?,
        )
    };
    let keep = token(cx).await?;
    let name = form.username;
    let outcome = st
        .db
        .run(move |c| {
            let Some(target) = users::by_name(c, &name)? else {
                return Ok(Notice::Invalid);
            };
            let own = target.id == me.id;
            if own && roles.admin != Some(true) {
                return Ok(Notice::SelfProtected);
            }
            users::update_roles(c, &target.id, &roles)?;
            if let Some((hash, sealed)) = credentials {
                users::set_password(c, &target.id, &hash, Some(&sealed))?;
                if own {
                    users::delete_sessions_except(c, &target.id, &keep)?;
                } else {
                    users::delete_sessions(c, &target.id)?;
                }
            }
            Ok(Notice::UserUpdated)
        })
        .await
        .map_err(fail)?;
    Ok(back(Page::Users, outcome))
}

#[derive(Deserialize)]
struct DeleteForm {
    username: String,
}

#[route(POST "/users/libraries")]
async fn user_libraries(
    cx: &Cx,
    Form(form): Form<std::collections::HashMap<String, String>>,
) -> Result<SeeOther> {
    require_admin(cx).await?;
    let Some(name) = form.get("username").cloned() else {
        return Ok(back(Page::Users, Notice::Invalid));
    };
    let granted: Vec<i64> = form
        .keys()
        .filter_map(|k| k.strip_prefix("lib_")?.parse().ok())
        .collect();
    let upload: Option<i64> = form.get("upload").and_then(|v| v.parse().ok());
    let outcome = state(cx)
        .db
        .run(move |c| {
            let Some(user) = users::by_name(c, &name)? else {
                return Ok(Notice::Invalid);
            };
            if !user.admin {
                crate::db::libraries::set_access(c, &user.id, &granted)?;
            }
            let upload = upload.filter(|id| {
                (user.admin || granted.contains(id))
                    && crate::db::libraries::get(c, *id).ok().flatten().is_some()
            });
            c.execute(
                "UPDATE users SET upload_library_id = ?2 WHERE id = ?1",
                rusqlite::params![user.id, upload],
            )?;
            Ok(Notice::AccessSaved)
        })
        .await
        .map_err(fail)?;
    Ok(back(Page::Users, outcome))
}

#[route(POST "/users/delete")]
async fn delete_user(cx: &Cx, Form(form): Form<DeleteForm>) -> Result<SeeOther> {
    let me = require_admin(cx).await?;
    let outcome = state(cx)
        .db
        .run(move |c| {
            let Some(target) = users::by_name(c, &form.username)? else {
                return Ok(Notice::Invalid);
            };
            if target.id == me.id {
                return Ok(Notice::SelfProtected);
            }
            users::delete(c, &target.id)?;
            Ok(Notice::UserDeleted)
        })
        .await
        .map_err(fail)?;
    Ok(back(Page::Users, outcome))
}

#[derive(Deserialize)]
struct ScanForm {
    full: Option<String>,
}

#[route(POST "/server/scan")]
async fn scan(cx: &Cx, Form(form): Form<ScanForm>) -> Result<SeeOther> {
    require_role(cx, User::can_scan).await?;
    let started = state(cx).scanner.start(form.full.is_some());
    Ok(back(
        Page::Server,
        if started {
            Notice::ScanStarted
        } else {
            Notice::ScanBusy
        },
    ))
}

#[derive(Deserialize)]
struct OutputForm {
    id: Option<String>,
    url: Option<String>,
}

#[route(POST "/server/jukebox/select")]
async fn jukebox_select(cx: &Cx, Form(form): Form<OutputForm>) -> Result<SeeOther> {
    require_role(cx, User::can_server).await?;
    let id = form.id.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let url = form.url.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let notice = match state(cx).jukebox.select(id, url).await {
        Ok(()) => Notice::Saved,
        Err(e) => {
            tracing::warn!("selecting jukebox output: {e}");
            Notice::JukeboxFailed
        }
    };
    Ok(back(Page::Server, notice))
}

#[route(POST "/server/jukebox/scan")]
async fn jukebox_scan(cx: &Cx) -> Result<SeeOther> {
    require_role(cx, User::can_server).await?;
    let notice = match state(cx).jukebox.scan().await {
        Ok(_) => Notice::JukeboxScanned,
        Err(_) => Notice::JukeboxFailed,
    };
    Ok(back(Page::Server, notice))
}

pub fn routes(b: RouterBuilder) -> RouterBuilder {
    b.route(profile)
        .route(password)
        .route(delete_key)
        .route(lastfm_start)
        .route(lastfm_callback)
        .route(lastfm_unlink)
        .route(create_user)
        .route(update_user)
        .route(user_libraries)
        .route(delete_user)
        .route(scan)
        .route(jukebox_select)
        .route(jukebox_scan)
}
