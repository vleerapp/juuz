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
    if matches!(
        action.as_str(),
        "set" | "add" | "remove" | "shuffle" | "start" | "skip"
    ) {
        jb.set_owner(&ctx.user().id).await;
    }
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
        "set" => jb.set(ids(&ctx)).await?,
        "add" => {
            let ids = ids(&ctx);
            if ids.is_empty() {
                return Err(ApiError::missing("id"));
            }
            jb.add(ids).await?
        }
        "clear" => jb.clear().await?,
        "remove" => jb.remove(index_param(&ctx, "index")?).await?,
        "shuffle" => jb.shuffle().await?,
        "start" => jb.start_playback().await?,
        "stop" => jb.stop_playback().await?,
        "skip" => {
            let index = index_param(&ctx, "index")?;
            let offset = match ctx.params.int("offset")? {
                Some(o) if o >= 0 => o as u64,
                Some(_) => return Err(ApiError::bad_param("offset")),
                None => 0,
            };
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

fn outputs_json(o: &Outputs) -> Value {
    let mut v = json!({
        "device": o.devices.iter().map(|d| json!({"id": d.udn, "name": d.name})).collect::<Vec<_>>(),
    });
    if let Some((d, online)) = &o.current {
        v["current"] = json!({"id": d.udn, "name": d.name, "online": online});
    }
    json!({"juuzJukeboxOutputs": v})
}

pub async fn get_outputs(ctx: Ctx) -> ApiResult {
    allowed(&ctx)?;
    ok(outputs_json(&ctx.st.jukebox.outputs().await))
}

pub async fn scan_outputs(ctx: Ctx) -> ApiResult {
    ctx.require_admin()?;
    ctx.st.jukebox.scan().await?;
    ok(outputs_json(&ctx.st.jukebox.outputs().await))
}

pub async fn set_output(ctx: Ctx) -> ApiResult {
    ctx.require_admin()?;
    let id = ctx.params.get("id").filter(|s| !s.is_empty());
    let url = ctx.params.get("url").filter(|s| !s.is_empty());
    ctx.st.jukebox.select(id, url).await?;
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
