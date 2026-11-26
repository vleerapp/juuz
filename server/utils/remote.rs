use crate::server::utils::db::users::User;
use crate::server::utils::error::ApiError;
use crate::server::utils::http_util::{cross_site, session_user};
use crate::server::utils::jukebox::group;
use crate::server::utils::jukebox::group::BROWSER_PREFIX;
use crate::server::utils::model::{songs_by_ids, songs_json};
use crate::server::utils::state::AppState;
use axum::Json;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn allowed(user: &User) -> bool {
    user.jukebox || user.admin
}

pub fn failed(e: ApiError) -> Response {
    let status = match e.code {
        10 => StatusCode::BAD_REQUEST,
        50 => StatusCode::FORBIDDEN,
        70 => StatusCode::NOT_FOUND,
        _ => StatusCode::BAD_GATEWAY,
    };
    (status, Json(json!({"error": e.message}))).into_response()
}

pub async fn caller(st: &AppState, headers: &HeaderMap, write: bool) -> Result<User, StatusCode> {
    if write && cross_site(headers) {
        return Err(StatusCode::FORBIDDEN);
    }
    let Some(user) = session_user(st, headers).await else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !allowed(&user) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(user)
}

pub async fn outputs_json(st: &AppState, user: &User) -> Value {
    let o = st.jukebox.outputs().await;
    let mut groups: Vec<Value> = Vec::new();
    for device in &o.devices {
        let current = o.current.as_ref().is_some_and(|(d, _)| d.udn == device.udn);
        let mut item = json!({"id": device.udn, "name": device.name, "current": current, "detail": device.detail()});
        if let Some(l) = o.locks.get(&device.udn) {
            item["lock"] =
                json!({"needs": l.needs, "configured": l.configured, "paired": l.paired});
        }
        match groups
            .iter_mut()
            .find(|g| g["protocol"] == device.protocol.as_str())
        {
            Some(group) => group["devices"].as_array_mut().expect("array").push(item),
            None => groups.push(json!({"protocol": device.protocol, "devices": [item]})),
        }
    }
    let current_udn = o.current.as_ref().map(|(d, _)| d.udn.as_str());
    let seats: Vec<Value> = st
        .jukebox
        .seats_of(&user.id)
        .into_iter()
        .map(|(id, info, kind)| {
            let udn = format!("{BROWSER_PREFIX}{id}");
            json!({
                "id": udn,
                "name": kind,
                "current": current_udn == Some(udn.as_str()),
                "detail": info,
            })
        })
        .collect();
    if !seats.is_empty() {
        groups.push(json!({"protocol": group::BROWSER, "devices": seats}));
    }
    let combos: Vec<Value> = o
        .groups
        .iter()
        .map(|g| {
            let d = g.device();
            json!({
                "id": d.udn,
                "name": g.name,
                "current": o.current.as_ref().is_some_and(|(c, _)| c.udn == d.udn),
                "members": g.members.iter().map(|m| json!({
                    "id": m.udn, "name": m.name, "protocol": m.protocol, "delayMs": m.delay_ms,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "current": o.current.as_ref().map(|(d, online)| json!({
            "id": d.udn, "name": d.name, "protocol": d.protocol, "online": online,
        })),
        "groups": groups,
        "combos": combos,
        "speakers": o.speakers,
        "bufferMs": crate::server::utils::jukebox::group::BUFFER_MS,
    })
}

#[derive(Deserialize)]
pub struct OutputQuery {
    pub id: Option<String>,
}

pub fn owns_seat(st: &AppState, user: &User, udn: &str) -> bool {
    st.jukebox
        .seats_of(&user.id)
        .iter()
        .any(|(id, _, _)| format!("{BROWSER_PREFIX}{id}") == udn)
}

pub async fn state_json(st: &AppState, user: &User) -> Result<Value, ApiError> {
    let (snap, queue) = st.jukebox.state().await;
    let uid = user.id.clone();
    let songs = st
        .db
        .run(move |c| {
            let rows = songs_by_ids(c, &uid, &queue)?;
            songs_json(c, &rows)
        })
        .await?;
    let outputs = st.jukebox.outputs().await;
    let clock = st.jukebox.clock().await;
    Ok(json!({
        "output": outputs.current.as_ref().map(|(d, online)| json!({
            "id": d.udn, "name": d.name, "protocol": d.protocol, "online": online,
        })),
        "index": snap.current_index,
        "playing": snap.playing,
        "position": snap.position,
        "positionMs": snap.position_ms,
        "clock": clock.map(|c| json!({"atUs": c.unix_us, "pos": c.pos})),
        "gain": snap.gain,
        "revision": snap.revision,
        "queue": songs,
    }))
}
