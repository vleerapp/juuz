use crate::server::utils::error::ApiError;
use crate::server::utils::params::Params;
use crate::server::utils::remote::{caller, failed, outputs_json};
use crate::server::utils::state::AppState;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

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
