use serde_json::json;

use super::{ApiResult, Ctx, ok};
use crate::error::ApiError;

pub async fn ping(_: Ctx) -> ApiResult {
    ok(json!({}))
}

pub async fn get_license(_: Ctx) -> ApiResult {
    ok(
        json!({"license": {"valid": true, "email": "", "licenseExpires": "2099-12-31T23:59:59.000Z"}}),
    )
}

pub async fn extensions(ctx: Ctx) -> ApiResult {
    let mut list = vec![
        json!({"name": "apiKeyAuthentication", "versions": [1]}),
        json!({"name": "formPost", "versions": [1]}),
        json!({"name": "songLyrics", "versions": [1]}),
        json!({"name": "indexBasedQueue", "versions": [1]}),
        json!({"name": "openMusicMetadata", "versions": [1]}),
        json!({"name": "juuzSongUpload", "versions": [1]}),
        json!({"name": "juuzJukeboxOutput", "versions": [1]}),
        json!({"name": "juuzJukeboxEvents", "versions": [1]}),
    ];
    if ctx.st.lastfm.is_some() {
        list.push(json!({"name": "juuzLastfm", "versions": [1]}));
    }
    ok(json!({"openSubsonicExtensions": list}))
}

pub async fn token_info(ctx: Ctx) -> ApiResult {
    ok(json!({"tokenInfo": {"username": ctx.user().name}}))
}

pub async fn unknown(_: Ctx) -> ApiResult {
    Err(ApiError::generic("unknown endpoint"))
}

pub async fn not_supported(_: Ctx) -> ApiResult {
    Err(ApiError::generic(
        "this operation is not supported by the server",
    ))
}

pub async fn not_found(_: Ctx) -> ApiResult {
    Err(ApiError::not_found("requested item"))
}
