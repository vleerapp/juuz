use serde_json::json;

use super::{ApiResult, Ctx, empty, ok};
use crate::auth;
use crate::db::users::{self, User};
use crate::error::ApiError;

fn require_password_auth(ctx: &Ctx) -> Result<(), ApiError> {
    if ctx.params.has("apiKey") {
        return Err(ApiError::forbidden(
            "this operation cannot be authorized with an api key",
        ));
    }
    Ok(())
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
            let folders = crate::db::libraries::visible_to(c, &u.id)?
                .into_iter()
                .map(|l| l.id)
                .collect::<Vec<_>>();
            Ok(u.to_json(&folders))
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
            let mut out = Vec::new();
            for u in users::list(c)? {
                let folders = crate::db::libraries::visible_to(c, &u.id)?
                    .into_iter()
                    .map(|l| l.id)
                    .collect::<Vec<_>>();
                out.push(u.to_json(&folders));
            }
            Ok(out)
        })
        .await?;
    ok(json!({"users": {"user": list}}))
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
    ctx.require(User::can_keys)?;
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
    ctx.require(User::can_keys)?;
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
    ctx.require(User::can_keys)?;
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
