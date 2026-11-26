use axum::http::header;
use axum::response::Response;

pub async fn safe_headers(mut resp: Response) -> Response {
    let headers = resp.headers_mut();
    headers
        .entry(header::X_CONTENT_TYPE_OPTIONS)
        .or_insert(axum::http::HeaderValue::from_static("nosniff"));
    headers
        .entry(header::REFERRER_POLICY)
        .or_insert(axum::http::HeaderValue::from_static("same-origin"));
    resp
}
