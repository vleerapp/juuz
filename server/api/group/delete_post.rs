use crate::server::utils::params::Params;
use crate::server::utils::remote::{caller, failed, outputs_json};
use crate::server::utils::state::AppState;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

pub async fn ungroup(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let user = match caller(&st, &headers, true).await {
        Ok(u) => u,
        Err(status) => return status.into_response(),
    };
    let p = Params::parse(&String::from_utf8_lossy(&body));
    let removed = async {
        let id = p.req("id")?;
        let id = id
            .strip_prefix(crate::server::utils::jukebox::group::PREFIX)
            .unwrap_or(id);
        st.jukebox.delete_group(id).await
    }
    .await;
    match removed {
        Ok(()) => Json(outputs_json(&st, &user).await).into_response(),
        Err(e) => failed(e),
    }
}
