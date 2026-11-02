use std::convert::Infallible;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::model::{songs_by_ids, songs_json};
use super::{ApiResult, Ctx, Reply, media, ok};
use crate::error::ApiError;
use crate::jukebox::{Outputs, Snapshot};
use crate::state::AppState;

fn allowed(ctx: &Ctx) -> Result<(), ApiError> {
    let user = ctx.user();
    if user.jukebox || user.admin {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "user is not authorized to control the jukebox",
        ))
    }
}

fn status_json(s: &Snapshot) -> Value {
    json!({
        "currentIndex": s.current_index,
        "playing": s.playing,
        "gain": s.gain,
        "position": s.position,
    })
}

fn index_param(ctx: &Ctx, key: &str) -> Result<usize, ApiError> {
    let raw = ctx.params.req(key)?;
    raw.parse::<usize>().map_err(|_| ApiError::bad_param(key))
}

fn ids(ctx: &Ctx) -> Vec<String> {
    ctx.params
        .all("id")
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub async fn control(ctx: Ctx) -> ApiResult {
    allowed(&ctx)?;
    let jb = ctx.st.jukebox.clone();
    let action = ctx.params.req("action")?.to_string();
    let uid = ctx.user().id.clone();
    let snap = match action.as_str() {
        "get" => {
            let (snap, queue) = jb.state().await;
            let uid = ctx.user().id.clone();
            let entry = ctx
                .st
                .db
                .run(move |c| {
                    let songs = songs_by_ids(c, &uid, &queue)?;
                    songs_json(c, &songs)
                })
                .await?;
            let mut playlist = status_json(&snap);
            if !entry.is_empty() {
                playlist["entry"] = Value::Array(entry);
            }
            return ok(json!({"jukeboxPlaylist": playlist}));
        }
        "status" => jb.snapshot().await,
        "set" => {
            let ids = ids(&ctx);
            visible_songs(&ctx, &ids).await?;
            jb.set_owner(&uid).await;
            jb.set(ids).await?
        }
        "add" => {
            let ids = ids(&ctx);
            if ids.is_empty() {
                return Err(ApiError::missing("id"));
            }
            visible_songs(&ctx, &ids).await?;
            jb.set_owner(&uid).await;
            jb.add(ids).await?
        }
        "clear" => jb.clear().await?,
        "remove" => {
            let index = index_param(&ctx, "index")?;
            jb.set_owner(&uid).await;
            jb.remove(index).await?
        }
        "shuffle" => {
            jb.set_owner(&uid).await;
            jb.shuffle().await?
        }
        "start" => {
            jb.set_owner(&uid).await;
            jb.start_playback().await?
        }
        "stop" => jb.stop_playback().await?,
        "skip" => {
            let index = index_param(&ctx, "index")?;
            let offset = match ctx.params.int("offset")? {
                Some(o) if o >= 0 => o as u64,
                Some(_) => return Err(ApiError::bad_param("offset")),
                None => 0,
            };
            jb.set_owner(&uid).await;
            jb.skip(index, offset).await?
        }
        "setGain" => {
            let gain: f64 = ctx
                .params
                .req("gain")?
                .parse()
                .map_err(|_| ApiError::bad_param("gain"))?;
            if !gain.is_finite() {
                return Err(ApiError::bad_param("gain"));
            }
            jb.set_gain(gain).await?
        }
        _ => return Err(ApiError::bad_param("action")),
    };
    ok(json!({"jukeboxStatus": status_json(&snap)}))
}

pub async fn move_item(ctx: Ctx) -> ApiResult {
    allowed(&ctx)?;
    let from = index_param(&ctx, "from")?;
    let to = index_param(&ctx, "to")?;
    let jb = ctx.st.jukebox.clone();
    jb.set_owner(&ctx.user().id).await;
    let snap = jb.move_item(from, to).await?;
    ok(json!({"jukeboxStatus": status_json(&snap)}))
}

fn group_json(g: &crate::jukebox::group::Group) -> Value {
    json!({
        "id": g.device().udn,
        "name": g.name,
        "protocol": g.device().protocol,
        "member": g.members.iter().map(|m| json!({
            "id": m.udn, "name": m.name, "protocol": m.protocol, "delayMs": m.delay_ms,
        })).collect::<Vec<_>>(),
    })
}

fn outputs_json(o: &Outputs) -> Value {
    let device = |d: &crate::jukebox::dlna::Device| {
        let mut v =
            json!({"id": d.udn, "name": d.name, "protocol": d.protocol, "detail": d.detail()});
        if let Some(l) = o.locks.get(&d.udn) {
            v["lock"] = json!({"needs": l.needs, "configured": l.configured, "paired": l.paired});
        }
        v
    };
    let mut v = json!({
        "device": o.devices.iter().map(device).collect::<Vec<_>>(),
        "group": o.groups.iter().map(group_json).collect::<Vec<_>>(),
        "speakers": o.speakers,
    });
    if let Some((d, online)) = &o.current {
        let mut current = device(d);
        current["online"] = json!(online);
        v["current"] = current;
    }
    json!({"juuzJukeboxOutputs": v})
}

pub async fn get_outputs(ctx: Ctx) -> ApiResult {
    allowed(&ctx)?;
    ok(outputs_json(&ctx.st.jukebox.outputs().await))
}

async fn visible_songs(ctx: &Ctx, ids: &[String]) -> Result<(), ApiError> {
    let (uid, ids) = (ctx.user().id.clone(), ids.to_vec());
    ctx.st
        .db
        .run(move |c| {
            for id in &ids {
                if !crate::db::libraries::can_access_song(c, &uid, id)? {
                    return Err(ApiError::not_found("song"));
                }
            }
            Ok(())
        })
        .await
}

pub async fn scan_outputs(ctx: Ctx) -> ApiResult {
    allowed(&ctx)?;
    ctx.st.jukebox.scan().await?;
    ok(outputs_json(&ctx.st.jukebox.outputs().await))
}

pub async fn set_output(ctx: Ctx) -> ApiResult {
    allowed(&ctx)?;
    let id = ctx.params.get("id").filter(|s| !s.is_empty());
    let url = ctx.params.get("url").filter(|s| !s.is_empty());
    if id.is_some_and(|id| id.starts_with(crate::jukebox::group::BROWSER_PREFIX)) {
        return Err(ApiError::bad_param("id"));
    }
    ctx.st.jukebox.select(id, url).await?;
    ok(outputs_json(&ctx.st.jukebox.outputs().await))
}

pub fn group_members(params: &crate::params::Params) -> Result<Vec<(String, i32)>, ApiError> {
    let delays = params.all("delay");
    params
        .all("member")
        .into_iter()
        .enumerate()
        .map(|(i, udn)| {
            let delay = match delays.get(i).filter(|d| !d.is_empty()) {
                Some(d) => d.parse::<i32>().map_err(|_| ApiError::bad_param("delay"))?,
                None => 0,
            };
            Ok((udn.to_string(), delay))
        })
        .collect()
}

pub async fn set_group(ctx: Ctx) -> ApiResult {
    allowed(&ctx)?;
    let id = ctx
        .params
        .get("id")
        .filter(|s| !s.is_empty())
        .map(|s| s.strip_prefix(crate::jukebox::group::PREFIX).unwrap_or(s));
    let name = ctx.params.req_text("name")?;
    let members = group_members(&ctx.params)?;
    ctx.st.jukebox.save_group(id, name, members).await?;
    ok(outputs_json(&ctx.st.jukebox.outputs().await))
}

pub async fn set_password(ctx: Ctx) -> ApiResult {
    allowed(&ctx)?;
    let id = ctx.params.req("id")?;
    let password = ctx.params.text("password")?.map(str::to_string);
    ctx.st.jukebox.airplay_password(id, password).await?;
    ok(outputs_json(&ctx.st.jukebox.outputs().await))
}

pub async fn start_pairing(ctx: Ctx) -> ApiResult {
    allowed(&ctx)?;
    ctx.st
        .jukebox
        .airplay_pair_start(ctx.params.req("id")?)
        .await?;
    ok(outputs_json(&ctx.st.jukebox.outputs().await))
}

pub async fn finish_pairing(ctx: Ctx) -> ApiResult {
    allowed(&ctx)?;
    let id = ctx.params.req("id")?;
    ctx.st
        .jukebox
        .airplay_pair_finish(id, ctx.params.req_text("pin")?)
        .await?;
    ok(outputs_json(&ctx.st.jukebox.outputs().await))
}

pub async fn forget_pairing(ctx: Ctx) -> ApiResult {
    allowed(&ctx)?;
    ctx.st.jukebox.airplay_forget(ctx.params.req("id")?).await?;
    ok(outputs_json(&ctx.st.jukebox.outputs().await))
}

pub async fn delete_group(ctx: Ctx) -> ApiResult {
    allowed(&ctx)?;
    let id = ctx.params.req("id")?;
    let id = id.strip_prefix(crate::jukebox::group::PREFIX).unwrap_or(id);
    ctx.st.jukebox.delete_group(id).await?;
    ok(outputs_json(&ctx.st.jukebox.outputs().await))
}

fn event(snap: &Snapshot) -> Event {
    Event::default()
        .event("status")
        .data(serde_json::to_string(snap).unwrap_or_default())
}

pub async fn events(ctx: Ctx) -> ApiResult {
    allowed(&ctx)?;
    let jb = ctx.st.jukebox.clone();
    let rx = jb.subscribe();
    let first = Some(jb.snapshot().await);
    let stop = jb.shutdown_signal();
    let stream =
        futures_util::stream::unfold((rx, stop, first), |(mut rx, mut stop, first)| async move {
            if let Some(snap) = first {
                return Some((Ok::<_, Infallible>(event(&snap)), (rx, stop, None)));
            }
            loop {
                if *stop.borrow() {
                    return None;
                }
                tokio::select! {
                    _ = stop.changed() => return None,
                    msg = rx.recv() => match msg {
                        Ok(snap) => return Some((Ok(event(&snap)), (rx, stop, None))),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                    },
                }
            }
        });
    let mut resp = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    resp.headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(Reply::Raw(resp))
}

pub async fn stream(
    State(st): State<AppState>,
    Path((id, file)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let sig = file.split('.').next().unwrap_or_default();
    if !st.jukebox.verify(&id, sig) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match media::serve_song(&st, &id, &headers).await {
        Ok(mut resp) => {
            let h = resp.headers_mut();
            h.insert(
                "transferMode.dlna.org",
                HeaderValue::from_static("Streaming"),
            );
            h.insert(
                "contentFeatures.dlna.org",
                HeaderValue::from_static(
                    "DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000",
                ),
            );
            resp
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}
