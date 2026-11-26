use crate::server::utils::db::users::User;
use crate::server::utils::db::{now, users};
use crate::server::utils::state::AppState;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use topcoat::session::Token;

#[derive(Deserialize)]
pub struct MediaQuery {
    pub id: String,
    pub size: Option<u32>,
}

pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(k, v)| (k == name).then_some(v))
}

pub fn cross_site(headers: &HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        return site != "same-origin";
    }
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let authority = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    !["x-forwarded-host", "host"].iter().any(|name| {
        headers
            .get(*name)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|h| h.split(',').next().map(str::trim) == Some(authority))
    })
}

pub fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = (bytes[i] == b'%' && i + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok())
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match decoded {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub async fn session_user(st: &AppState, headers: &HeaderMap) -> Option<User> {
    let raw = cookie(headers, crate::app::COOKIE)?;
    let token = Token::decode(&percent_decode(raw)).ok()?;
    let key = token.hash().to_vec();
    st.db
        .run(move |c| Ok(users::by_session(c, &key, now())?))
        .await
        .ok()
        .flatten()
}

pub fn denied() -> Response {
    StatusCode::UNAUTHORIZED.into_response()
}

pub fn failed(e: crate::server::utils::error::ApiError) -> Response {
    let status = if e.code == 70 {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::BAD_REQUEST
    };
    status.into_response()
}
