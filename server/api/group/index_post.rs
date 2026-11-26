use crate::server::utils::error::ApiError;
use crate::server::utils::jukebox::group::BROWSER_PREFIX;
use crate::server::utils::params::Params;
use crate::server::utils::remote::{caller, failed, outputs_json, owns_seat};
use crate::server::utils::state::AppState;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::json;

pub async fn group(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let user = match caller(&st, &headers, true).await {
        Ok(u) => u,
        Err(status) => return status.into_response(),
    };
    let p = Params::parse(&String::from_utf8_lossy(&body));
    let saved = async {
        let id = p.get("id").filter(|s| !s.is_empty()).map(|s| {
            s.strip_prefix(crate::server::utils::jukebox::group::PREFIX)
                .unwrap_or(s)
        });
        let members = crate::server::api::rest::jukebox::group_members(&p)?;
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
