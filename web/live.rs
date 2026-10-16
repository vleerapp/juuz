use std::time::Duration;

use tokio::sync::broadcast::error::RecvError;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::websocket::{Message, WebSocketUpgrade};
use topcoat::router::response::Response;
use topcoat::router::route;

use super::auth::{require, state};

const COALESCE: Duration = Duration::from_millis(40);

#[route(GET "/events")]
async fn events(cx: &Cx, upgrade: WebSocketUpgrade) -> Result<Response> {
    require(cx).await?;
    let mut changes = state(cx).live.subscribe();
    upgrade.on_upgrade(move |mut socket| async move {
        loop {
            tokio::select! {
                change = changes.recv() => match change {
                    Ok(()) | Err(RecvError::Lagged(_)) => {
                        tokio::time::sleep(COALESCE).await;
                        while changes.try_recv().is_ok() {}
                        if socket.send(Message::text("pulse")).await.is_err() {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                },
                incoming = socket.recv() => match incoming {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
            }
        }
    })
}

pub fn routes(b: topcoat::router::RouterBuilder) -> topcoat::router::RouterBuilder {
    b.route(events)
}
