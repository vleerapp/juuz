use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::server::utils::db::now;
use crate::server::utils::error::ApiError;
use crate::server::utils::http_util::{cross_site, denied, failed, session_user};
use crate::server::utils::model::kind_of_for;
use crate::server::utils::state::AppState;

#[derive(Deserialize)]
pub struct StarQuery {
    pub id: String,
    pub on: bool,
}

pub async fn star(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<StarQuery>,
) -> Response {
    if cross_site(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(user) = session_user(&st, &headers).await else {
        return denied();
    };
    let uid = user.id.clone();
    let result = st
        .db
        .run(move |c| {
            if kind_of_for(c, &uid, &q.id)?.is_none() {
                return Err(ApiError::not_found("item"));
            }
            if q.on {
                c.execute(
                    "INSERT OR IGNORE INTO starred(user_id, item_id, starred_at) VALUES (?1, ?2, ?3)",
                    rusqlite::params![uid, q.id, now()],
                )?;
            } else {
                c.execute(
                    "DELETE FROM starred WHERE user_id = ?1 AND item_id = ?2",
                    rusqlite::params![uid, q.id],
                )?;
            }
            Ok(())
        })
        .await;
    match result {
        Ok(()) => {
            st.live.notify();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => failed(e),
    }
}
