pub mod account;
pub mod airplay_post;
pub mod art_get;
pub mod events_get;
pub mod group;
pub mod library;
pub mod login_post;
pub mod logout_post;
pub mod output_post;
pub mod outputs;
pub mod play_get;
pub mod playback_post;
pub mod queue_get;
pub mod queue_post;
pub mod rest;
pub mod scrobble_post;
pub mod server;
pub mod users;

use axum::Router;
use axum::extract::Request;

use crate::server::utils::state::AppState;

pub fn router(st: AppState) -> Router {
    let mut r = Router::new();
    for (name, handler) in rest::endpoints() {
        r = r
            .route(&format!("/rest/{name}"), handler.clone())
            .route(&format!("/rest/{name}.view"), handler);
    }
    let web = crate::app::service(st.clone());
    for path in crate::app::PATHS {
        r = r.route_service(path, web.clone());
    }
    r.route("/play", axum::routing::get(play_get::play))
        .route("/art", axum::routing::get(art_get::art))
        .route(
            "/scrobble",
            axum::routing::post(scrobble_post::scrobble),
        )
        .route("/playback", axum::routing::post(playback_post::event))
        .route("/outputs", axum::routing::get(outputs::index_get::outputs))
        .route("/outputs/scan", axum::routing::post(outputs::scan_post::scan))
        .route("/output", axum::routing::post(output_post::select))
        .route("/airplay", axum::routing::post(airplay_post::airplay))
        .route("/group", axum::routing::post(group::index_post::group))
        .route(
            "/group/delete",
            axum::routing::post(group::delete_post::ungroup),
        )
        .route("/queue", axum::routing::get(queue_get::state))
        .route("/queue", axum::routing::post(queue_post::queue))
        .route(
            "/jukebox/stream/{id}/{file}",
            axum::routing::get(rest::jukebox::stream),
        )
        .route("/share/{id}", axum::routing::get(rest::social::public_share))
        .route(
            "/share/{id}/stream/{song}",
            axum::routing::get(rest::social::public_share_stream),
        )
        .fallback(rest::endpoint(false, rest::system::unknown))
        .layer(axum::middleware::from_fn_with_state(st.clone(), crate::server::middleware::guard::guard))
        .layer(axum::middleware::map_response(crate::server::middleware::headers::safe_headers))
        .layer(tower_http::cors::CorsLayer::permissive())
        .layer(
            tower_http::trace::TraceLayer::new_for_http().make_span_with(|req: &Request| {
                tracing::debug_span!("request", method = %req.method(), path = %req.uri().path())
            }),
        )
        .with_state(st)
}
