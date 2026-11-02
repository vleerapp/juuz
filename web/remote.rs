use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::media::{cross_site, session_user};
use crate::api::model::{songs_by_ids, songs_json};
use crate::db::users::User;
use crate::error::ApiError;
use crate::jukebox::group::{self, BROWSER_PREFIX};
use crate::params::Params;
use crate::state::AppState;

pub(super) fn allowed(user: &User) -> bool {
    user.jukebox || user.admin
}

fn failed(e: ApiError) -> Response {
    let status = match e.code {
        10 => StatusCode::BAD_REQUEST,
        50 => StatusCode::FORBIDDEN,
        70 => StatusCode::NOT_FOUND,
        _ => StatusCode::BAD_GATEWAY,
    };
    (status, Json(json!({"error": e.message}))).into_response()
}

async fn caller(st: &AppState, headers: &HeaderMap, write: bool) -> Result<User, StatusCode> {
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

pub(super) async fn outputs_json(st: &AppState, user: &User) -> Value {
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
        "bufferMs": crate::jukebox::group::BUFFER_MS,
    })
}

pub async fn outputs(State(st): State<AppState>, headers: HeaderMap) -> Response {
    match caller(&st, &headers, false).await {
        Ok(user) => Json(outputs_json(&st, &user).await).into_response(),
        Err(status) => status.into_response(),
    }
}

#[derive(Deserialize)]
pub struct OutputQuery {
    id: Option<String>,
}

fn owns_seat(st: &AppState, user: &User, udn: &str) -> bool {
    st.jukebox
        .seats_of(&user.id)
        .iter()
        .any(|(id, _, _)| format!("{BROWSER_PREFIX}{id}") == udn)
}

pub async fn select(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<OutputQuery>,
) -> Response {
    let user = match caller(&st, &headers, true).await {
        Ok(u) => u,
        Err(status) => return status.into_response(),
    };
    let id = q.id.as_deref().filter(|s| !s.is_empty());
    if id.is_some_and(|id| id.starts_with(BROWSER_PREFIX) && !owns_seat(&st, &user, id)) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match st.jukebox.select(id, None).await {
        Ok(()) => Json(outputs_json(&st, &user).await).into_response(),
        Err(e) => {
            tracing::warn!("selecting jukebox output: {e}");
            failed(e)
        }
    }
}

pub async fn group(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let user = match caller(&st, &headers, true).await {
        Ok(u) => u,
        Err(status) => return status.into_response(),
    };
    let p = Params::parse(&String::from_utf8_lossy(&body));
    let saved = async {
        let id = p
            .get("id")
            .filter(|s| !s.is_empty())
            .map(|s| s.strip_prefix(crate::jukebox::group::PREFIX).unwrap_or(s));
        let members = crate::api::jukebox::group_members(&p)?;
        let before = st.jukebox.outputs().await;
        let kept: Vec<&str> = id
            .and_then(|id| before.groups.iter().find(|g| g.id == id))
            .map(|g| g.members.iter().map(|m| m.udn.as_str()).collect())
            .unwrap_or_default();
        for (udn, _) in &members {
            if udn.starts_with(BROWSER_PREFIX)
                && !owns_seat(&st, &user, udn)
                && !kept.contains(&udn.as_str())
            {
                return Err(ApiError::forbidden("not your browser"));
            }
        }
        st.jukebox
            .save_group(id, p.req_text("name")?, members)
            .await
    }
    .await;
    match saved {
        Ok(g) => {
            let mut v = outputs_json(&st, &user).await;
            v["saved"] = json!(g.device().udn);
            Json(v).into_response()
        }
        Err(e) => failed(e),
    }
}

pub async fn airplay(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let user = match caller(&st, &headers, true).await {
        Ok(u) => u,
        Err(status) => return status.into_response(),
    };
    let p = Params::parse(&String::from_utf8_lossy(&body));
    let done = async {
        let id = p.req("id")?;
        match p.req("action")? {
            "password" => {
                let password = p.text("password")?.map(str::to_string);
                st.jukebox.airplay_password(id, password).await
            }
            "pair" => st.jukebox.airplay_pair_start(id).await,
            "finish" => st.jukebox.airplay_pair_finish(id, p.req_text("pin")?).await,
            "forget" => st.jukebox.airplay_forget(id).await,
            _ => Err(ApiError::bad_param("action")),
        }
    }
    .await;
    match done {
        Ok(()) => Json(outputs_json(&st, &user).await).into_response(),
        Err(e) => failed(e),
    }
}

pub async fn ungroup(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let user = match caller(&st, &headers, true).await {
        Ok(u) => u,
        Err(status) => return status.into_response(),
    };
    let p = Params::parse(&String::from_utf8_lossy(&body));
    let removed = async {
        let id = p.req("id")?;
        let id = id.strip_prefix(crate::jukebox::group::PREFIX).unwrap_or(id);
        st.jukebox.delete_group(id).await
    }
    .await;
    match removed {
        Ok(()) => Json(outputs_json(&st, &user).await).into_response(),
        Err(e) => failed(e),
    }
}

pub async fn scan(State(st): State<AppState>, headers: HeaderMap) -> Response {
    let user = match caller(&st, &headers, true).await {
        Ok(u) => u,
        Err(status) => return status.into_response(),
    };
    match st.jukebox.scan().await {
        Ok(_) => Json(outputs_json(&st, &user).await).into_response(),
        Err(e) => failed(e),
    }
}

pub(super) async fn state_json(st: &AppState, user: &User) -> Result<Value, ApiError> {
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

pub async fn state(State(st): State<AppState>, headers: HeaderMap) -> Response {
    let user = match caller(&st, &headers, false).await {
        Ok(u) => u,
        Err(status) => return status.into_response(),
    };
    match state_json(&st, &user).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => failed(e),
    }
}

pub async fn queue(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let user = match caller(&st, &headers, true).await {
        Ok(u) => u,
        Err(status) => return status.into_response(),
    };
    let p = Params::parse(&String::from_utf8_lossy(&body));
    let jb = &st.jukebox;
    let ids: Vec<String> = p.all("id").into_iter().map(str::to_string).collect();
    let number = |key: &str| -> Result<usize, ApiError> {
        p.req(key)?
            .parse::<usize>()
            .map_err(|_| ApiError::bad_param(key))
    };
    let result = async {
        if !ids.is_empty() {
            let uid = user.id.clone();
            let check = ids.clone();
            st.db
                .run(move |c| {
                    for id in &check {
                        if !crate::db::libraries::can_access_song(c, &uid, id)? {
                            return Err(ApiError::not_found("song"));
                        }
                    }
                    Ok(())
                })
                .await?;
        }
        match p.req("action")? {
            "set" => {
                let index = number("index").unwrap_or(0);
                let offset = number("offset").unwrap_or(0) as u64;
                let play = p.get("play") != Some("0");
                jb.set_owner(&user.id).await;
                jb.set(ids).await?;
                jb.skip(index, offset).await?;
                if play {
                    jb.start_playback().await
                } else {
                    Ok(jb.snapshot().await)
                }
            }
            "add" => {
                jb.set_owner(&user.id).await;
                jb.add(ids).await
            }
            "remove" => jb.remove(number("index")?).await,
            "move" => jb.move_item(number("from")?, number("to")?).await,
            "clear" => jb.clear().await,
            "skip" => {
                let index = number("index")?;
                let offset = number("offset").unwrap_or(0) as u64;
                jb.set_owner(&user.id).await;
                jb.skip(index, offset).await?;
                jb.start_playback().await
            }
            "start" => {
                jb.set_owner(&user.id).await;
                jb.start_playback().await
            }
            "stop" => jb.stop_playback().await,
            "gain" => {
                let gain: f64 = p
                    .req("gain")?
                    .parse()
                    .map_err(|_| ApiError::bad_param("gain"))?;
                if !gain.is_finite() {
                    return Err(ApiError::bad_param("gain"));
                }
                jb.set_gain(gain).await
            }
            _ => Err(ApiError::bad_param("action")),
        }
    }
    .await;
    match result {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => failed(e),
    }
}
