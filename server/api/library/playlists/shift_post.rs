use crate::server::utils::playlists::{MoveForm, shift};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::route;

#[route(POST "/library/playlists/shift")]
pub async fn shifted(cx: &Cx, Form(form): Form<MoveForm>) -> Result<Response> {
    Ok(if shift(cx, form).await? {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::CONFLICT
    }
    .into_response())
}
