use serde_json::json;

use super::browse::FOLDER_IDS;
use super::{ApiResult, Ctx, empty, ok};
use crate::auth;
use crate::db::users::{self, RoleUpdate, User};
use crate::error::ApiError;
use crate::params::Params;

fn roles_from(p: &Params) -> RoleUpdate {
    RoleUpdate {
        admin: p.bool("adminRole"),
        settings: p.bool("settingsRole"),
        stream: p.bool("streamRole"),
        download: p.bool("downloadRole"),
        upload: p.bool("uploadRole"),
        playlist: p.bool("playlistRole"),
        cover_art: p.bool("coverArtRole"),
        comment: p.bool("commentRole"),
        podcast: p.bool("podcastRole"),
        share: p.bool("shareRole"),
        jukebox: p.bool("jukeboxRole"),
        scrobbling: p.bool("scrobblingEnabled"),
        max_bit_rate: p.get("maxBitRate").and_then(|v| v.parse().ok()),
        email: p.get("email").map(str::to_string),
    }
}

fn require_password_auth(ctx: &Ctx) -> Result<(), ApiError> {
    if ctx.params.has("apiKey") {
        return Err(ApiError::forbidden(
            "this operation cannot be authorized with an api key",
        ));
    }
    Ok(())
}

fn decode(raw: &str) -> Result<String, ApiError> {
    match raw.strip_prefix("enc:") {
        Some(h) => String::from_utf8(hex::decode(h).map_err(|_| ApiError::bad_param("password"))?)
            .map_err(|_| ApiError::bad_param("password")),
        None => Ok(raw.to_string()),
    }
}

pub async fn get_user(ctx: Ctx) -> ApiResult {
    let me = ctx.user().clone();
    let target = ctx.params.get("username").unwrap_or(&me.name).to_string();
    if !target.eq_ignore_ascii_case(&me.name) && !me.admin {
        return Err(ApiError::forbidden(
            "user is not authorized for the given operation",
        ));
    }
    let user = ctx
        .st
        .db
        .run(move |c| {
            let u = users::by_name(c, &target)?.ok_or_else(|| ApiError::not_found("user"))?;
            Ok(u.to_json(&FOLDER_IDS))
        })
        .await?;
    ok(json!({"user": user}))
}

pub async fn get_users(ctx: Ctx) -> ApiResult {
    ctx.require_admin()?;
    let list = ctx
        .st
        .db
        .run(|c| {
            Ok(users::list(c)?
                .iter()
                .map(|u| u.to_json(&FOLDER_IDS))
                .collect::<Vec<_>>())
        })
        .await?;
    ok(json!({"users": {"user": list}}))
}

pub async fn create_user(ctx: Ctx) -> ApiResult {
    ctx.require_admin()?;
    let name = ctx.params.req("username")?.to_string();
    let password = decode(ctx.params.req("password")?)?;
    let roles = roles_from(&ctx.params);

    let (hash, secret) = auth::credentials(&ctx.st.crypto, &password).await?;

    ctx.st
        .db
        .run(move |c| {
            let id = users::create(c, &name, &hash, Some(&secret), false)?;
            users::update_roles(c, &id, &roles)?;
            Ok(())
        })
        .await?;
    empty()
}

pub async fn update_user(ctx: Ctx) -> ApiResult {
    ctx.require_admin()?;
    let name = ctx.params.req("username")?.to_string();
    let roles = roles_from(&ctx.params);
    let me = ctx.user().clone();
    if ctx.params.has("password") {
        require_password_auth(&ctx)?;
    }
    let new_password = ctx.params.get("password").map(decode).transpose()?;

    let credentials = match &new_password {
        Some(pw) => Some(auth::credentials(&ctx.st.crypto, pw).await?),
        None => None,
    };

    ctx.st
        .db
        .run(move |c| {
            let user: User =
                users::by_name(c, &name)?.ok_or_else(|| ApiError::not_found("user"))?;
            if user.id == me.id && roles.admin == Some(false) {
                return Err(ApiError::forbidden("you cannot remove your own admin role"));
            }
            users::update_roles(c, &user.id, &roles)?;
            if let Some((hash, sealed)) = credentials {
                users::set_password(c, &user.id, &hash, Some(&sealed))?;
                users::delete_sessions(c, &user.id)?;
            }
            Ok(())
        })
        .await?;
    empty()
}

pub async fn delete_user(ctx: Ctx) -> ApiResult {
    ctx.require_admin()?;
    let name = ctx.params.req("username")?.to_string();
    let me = ctx.user().id.clone();
    ctx.st
        .db
        .run(move |c| {
            let user = users::by_name(c, &name)?.ok_or_else(|| ApiError::not_found("user"))?;
            if user.id == me {
                return Err(ApiError::forbidden("you cannot delete your own account"));
            }
            users::delete(c, &user.id)?;
            Ok(())
        })
        .await?;
    empty()
}

pub async fn change_password(ctx: Ctx) -> ApiResult {
    require_password_auth(&ctx)?;
    let me = ctx.user().clone();
    let name = ctx.params.req("username")?.to_string();
    if !name.eq_ignore_ascii_case(&me.name) && !me.admin {
        return Err(ApiError::forbidden(
            "user is not authorized for the given operation",
        ));
    }
    let password = decode(ctx.params.req("password")?)?;
    let (hash, sealed) = auth::credentials(&ctx.st.crypto, &password).await?;
    ctx.st
        .db
        .run(move |c| {
            let user = users::by_name(c, &name)?.ok_or_else(|| ApiError::not_found("user"))?;
            users::set_password(c, &user.id, &hash, Some(&sealed))?;
            users::delete_sessions(c, &user.id)?;
            Ok(())
        })
        .await?;
    empty()
}

async fn key_owner(ctx: &Ctx) -> Result<User, ApiError> {
    let me = ctx.user().clone();
    match ctx.params.get("username") {
        Some(name) if !name.eq_ignore_ascii_case(&me.name) => {
            ctx.require_admin()?;
            let name = name.to_string();
            ctx.st
                .db
                .run(move |c| users::by_name(c, &name)?.ok_or_else(|| ApiError::not_found("user")))
                .await
        }
        _ => Ok(me),
    }
}

pub async fn create_api_key(ctx: Ctx) -> ApiResult {
    require_password_auth(&ctx)?;
    let owner = key_owner(&ctx).await?;
    let label = ctx.params.get("name").unwrap_or("").to_string();
    let key = auth::generate_api_key();
    let hash = auth::hash_api_key(&key);
    let (user_id, name) = (owner.id.clone(), label.clone());
    let id = ctx
        .st
        .db
        .run(move |c| Ok(users::create_api_key(c, &user_id, &name, &hash)?))
        .await?;
    ok(json!({"juuzApiKey": {"id": id, "name": label, "username": owner.name, "key": key}}))
}

pub async fn get_api_keys(ctx: Ctx) -> ApiResult {
    let owner = key_owner(&ctx).await?;
    let keys = ctx
        .st
        .db
        .run(move |c| Ok(users::list_api_keys(c, &owner.id)?))
        .await?;
    let list: Vec<_> = keys
        .into_iter()
        .map(|(id, name, created)| json!({"id": id, "name": name, "created": crate::util::iso(created)}))
        .collect();
    ok(json!({"juuzApiKeys": {"apiKey": list}}))
}

pub async fn delete_api_key(ctx: Ctx) -> ApiResult {
    let owner = key_owner(&ctx).await?;
    let id = ctx.params.req("id")?.to_string();
    let removed = ctx
        .st
        .db
        .run(move |c| Ok(users::delete_api_key(c, &owner.id, &id)?))
        .await?;
    if removed == 0 {
        return Err(ApiError::not_found("api key"));
    }
    empty()
}
