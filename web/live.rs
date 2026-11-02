use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::websocket::{Message, WebSocketUpgrade};
use topcoat::router::response::Response;
use topcoat::router::{query_params, route};

use super::auth::{require, state};
use super::remote;

const COALESCE: Duration = Duration::from_millis(40);

fn frame(kind: &str, data: Value) -> String {
    json!({"t": kind, "d": data}).to_string()
}

type Pcm = tokio::sync::broadcast::Receiver<std::sync::Arc<Vec<u8>>>;

async fn next_pcm(rx: &mut Option<Pcm>) -> Result<std::sync::Arc<Vec<u8>>, RecvError> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

#[query_params(error = bad_request)]
struct Seat {
    b: Option<String>,
    d: Option<String>,
    k: Option<String>,
}

struct Leave {
    st: crate::state::AppState,
    id: Option<String>,
}

impl Drop for Leave {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.st.jukebox.seat_leave(&id);
        }
    }
}

#[route(GET "/events")]
async fn events(cx: &Cx, upgrade: WebSocketUpgrade) -> Result<Response> {
    let user = require(cx).await?;
    let seat = query_params::<Seat>(cx)?;
    let st = state(cx).clone();
    let mut changes = st.live.subscribe();
    let mut snaps = st.jukebox.subscribe();
    let jukebox = remote::allowed(&user);
    let seated = match seat.b.as_deref() {
        Some(id) if jukebox => st
            .jukebox
            .seat_join(
                &user.id,
                id,
                seat.d.as_deref().unwrap_or_default(),
                seat.k.as_deref().unwrap_or_default(),
            )
            .then(|| id.to_string()),
        _ => None,
    };
    let mut pcm: Option<Pcm> = seated.as_deref().map(|id| st.jukebox.pcm(id).subscribe());
    let leave = Leave {
        st: st.clone(),
        id: seated,
    };
    upgrade.on_upgrade(move |mut socket| async move {
        let _leave = leave;
        let mut last: [String; 2] = Default::default();
        let mut dirty = true;
        let mut page = false;
        let mut retry = false;
        let mut tries = 0u8;
        loop {
            if dirty {
                let mut out: Vec<(Option<usize>, String)> = Vec::new();
                if page {
                    out.push((None, "pulse".to_string()));
                }
                if jukebox {
                    if let Ok(queue) = remote::state_json(&st, &user).await {
                        let waiting = queue["playing"] == true && queue["clock"].is_null();
                        retry = waiting && tries < 8;
                        tries = if waiting { tries + 1 } else { 0 };
                        out.push((Some(0), frame("queue", queue)));
                    }
                    out.push((
                        Some(1),
                        frame("outputs", remote::outputs_json(&st, &user).await),
                    ));
                }
                for (slot, text) in out {
                    if let Some(i) = slot {
                        if last[i] == text {
                            continue;
                        }
                        last[i].clone_from(&text);
                    }
                    if socket.send(Message::text(text)).await.is_err() {
                        return;
                    }
                }
                dirty = false;
                page = false;
            }
            tokio::select! {
                change = changes.recv() => match change {
                    Ok(()) | Err(RecvError::Lagged(_)) => {
                        tokio::time::sleep(COALESCE).await;
                        while changes.try_recv().is_ok() {}
                        dirty = true;
                        page = true;
                    }
                    Err(RecvError::Closed) => return,
                },
                frame = next_pcm(&mut pcm) => match frame {
                    Ok(frame) => {
                        let bytes = axum::body::Bytes::from(frame.as_ref().clone());
                        if socket.send(Message::Binary(bytes)).await.is_err() {
                            return;
                        }
                    }
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => pcm = None,
                },
                _ = tokio::time::sleep(Duration::from_millis(250)), if retry => {
                    retry = false;
                    dirty = true;
                }
                snap = snaps.recv(), if jukebox => match snap {
                    Ok(_) | Err(RecvError::Lagged(_)) => {
                        tokio::time::sleep(COALESCE).await;
                        while snaps.try_recv().is_ok() {}
                        dirty = true;
                    }
                    Err(RecvError::Closed) => return,
                },
                incoming = socket.recv() => match incoming {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                    Some(Ok(Message::Text(text))) => {
                        if let Some(echo) = text.as_str().strip_prefix("sync:") {
                            let now = crate::jukebox::stream::unix_us() as f64 / 1000.0;
                            let reply = json!({"t": "sync", "c": echo, "s": now}).to_string();
                            if socket.send(Message::text(reply)).await.is_err() {
                                return;
                            }
                        }
                    }
                    Some(Ok(_)) => {}
                },
            }
        }
    })
}

pub fn routes(b: topcoat::router::RouterBuilder) -> topcoat::router::RouterBuilder {
    b.route(events)
}
