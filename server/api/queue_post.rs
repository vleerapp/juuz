use crate::server::utils::error::ApiError;
use crate::server::utils::params::Params;
use crate::server::utils::remote::{caller, failed};
use crate::server::utils::state::AppState;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

pub async fn queue(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let user = match caller(&st, &headers, true).await {
        Ok(u) => u,
        Err(status) => return status.into_response(),
    };
    let p = Params::parse(&String::from_utf8_lossy(&body));
    let jb = &st.jukebox;
    let ids: Vec<String> = p.all("id").into_iter().map(str::to_string).collect();
    let number = |key: &str| -> Result<usize, ApiError> {
        p.req(key)?
            .parse::<usize>()
            .map_err(|_| ApiError::bad_param(key))
    };
    let result = async {
        if !ids.is_empty() {
            let uid = user.id.clone();
            let check = ids.clone();
            st.db
                .run(move |c| {
                    for id in &check {
                        if !crate::server::utils::db::libraries::can_access_song(c, &uid, id)? {
                            return Err(ApiError::not_found("song"));
                        }
                    }
                    Ok(())
                })
                .await?;
        }
        match p.req("action")? {
            "set" => {
                let index = number("index").unwrap_or(0);
                let offset = number("offset").unwrap_or(0) as u64;
                let play = p.get("play") != Some("0");
                jb.set_owner(&user.id).await;
                jb.set(ids).await?;
                jb.skip(index, offset).await?;
                if play {
                    jb.start_playback().await
                } else {
                    Ok(jb.snapshot().await)
                }
            }
            "add" => {
                jb.set_owner(&user.id).await;
                jb.add(ids).await
            }
            "remove" => jb.remove(number("index")?).await,
            "move" => jb.move_item(number("from")?, number("to")?).await,
            "clear" => jb.clear().await,
            "skip" => {
                let index = number("index")?;
                let offset = number("offset").unwrap_or(0) as u64;
                jb.set_owner(&user.id).await;
                jb.skip(index, offset).await?;
                jb.start_playback().await
            }
            "start" => {
                jb.set_owner(&user.id).await;
                jb.start_playback().await
            }
            "stop" => jb.stop_playback().await,
            "gain" => {
                let gain: f64 = p
                    .req("gain")?
                    .parse()
                    .map_err(|_| ApiError::bad_param("gain"))?;
                if !gain.is_finite() {
                    return Err(ApiError::bad_param("gain"));
                }
                jb.set_gain(gain).await
            }
            _ => Err(ApiError::bad_param("action")),
        }
    }
    .await;
    match result {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => failed(e),
    }
}
