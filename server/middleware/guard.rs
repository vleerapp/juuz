use crate::server::utils::error::ApiError;
use crate::server::utils::params::Params;
use crate::server::utils::response::{Format, render};
use crate::server::utils::state::AppState;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;

pub async fn guard(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip());
    let ip = super::rate_limit::resolve_ip(&st.cfg.ratelimit, peer, req.headers());
    let key = super::rate_limit::key_for(ip);
    if let Ok(value) = axum::http::HeaderValue::from_str(&key) {
        req.headers_mut()
            .insert(super::rate_limit::CLIENT_IP_HEADER, value);
    }
    if let Err(retry) = st.ratelimit.allow(&key) {
        return too_many(&req, retry);
    }
    next.run(req).await
}

fn too_many(req: &Request, retry: u64) -> Response {
    let mut resp = if req.uri().path().starts_with("/rest/") {
        let params = Params::parse(req.uri().query().unwrap_or(""));
        render(
            &Format::from_params(&params),
            Err(&ApiError::generic("rate limit exceeded, slow down")),
        )
    } else {
        Response::new(Body::from("rate limit exceeded"))
    };
    *resp.status_mut() = axum::http::StatusCode::TOO_MANY_REQUESTS;
    if let Ok(v) = axum::http::HeaderValue::from_str(&retry.to_string()) {
        resp.headers_mut().insert(header::RETRY_AFTER, v);
    }
    resp
}
