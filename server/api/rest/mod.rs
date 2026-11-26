pub mod annotation;
pub mod browse;
pub mod jukebox;
pub mod lastfm;
pub mod listenbrainz;
pub mod lists;
pub mod media;
pub mod playlists;
pub mod queue;
pub mod scrobble;
pub mod search;
pub mod social;
pub mod system;
pub mod upload;
pub mod users;

use std::future::Future;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, header};
use axum::response::Response;
use axum::routing::{MethodRouter, any};
use serde_json::Value;

use crate::server::utils::auth;
use crate::server::utils::db::users::User;
use crate::server::utils::error::ApiError;
use crate::server::utils::params::Params;
use crate::server::utils::response::{Format, render};
use crate::server::utils::state::AppState;

pub struct Ctx {
    pub st: AppState,
    pub params: Params,
    pub user: Option<User>,
    pub headers: HeaderMap,
    body: std::sync::Mutex<Option<Body>>,
}

impl Ctx {
    pub fn take_body(&self) -> Option<Body> {
        self.body.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn user(&self) -> &User {
        self.user
            .as_ref()
            .expect("endpoint requires authentication")
    }

    pub fn require(&self, allowed: fn(&User) -> bool) -> Result<(), ApiError> {
        if allowed(self.user()) {
            Ok(())
        } else {
            Err(ApiError::forbidden(
                "user is not authorized for the given operation",
            ))
        }
    }

    pub fn require_admin(&self) -> Result<(), ApiError> {
        if self.user().admin {
            Ok(())
        } else {
            Err(ApiError::forbidden(
                "user is not authorized for the given operation",
            ))
        }
    }
}

pub enum Reply {
    Data(Value),
    Empty,
    Raw(Response),
}

pub type ApiResult = Result<Reply, ApiError>;

pub fn ok(v: Value) -> ApiResult {
    Ok(Reply::Data(v))
}

pub fn empty() -> ApiResult {
    Ok(Reply::Empty)
}

const FORM_LIMIT: usize = 4 * 1024 * 1024;

async fn run<F, Fut>(st: AppState, req: Request, authed: bool, f: F) -> Response
where
    F: Fn(Ctx) -> Fut,
    Fut: Future<Output = ApiResult>,
{
    let (parts, body) = req.into_parts();
    let mut params = Params::parse(parts.uri.query().unwrap_or(""));
    let is_form = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"));

    let mut body = Some(body);
    if is_form {
        let raw = body.take().expect("body present");
        match axum::body::to_bytes(raw, FORM_LIMIT).await {
            Ok(bytes) => params.extend(Params::parse(&String::from_utf8_lossy(&bytes))),
            Err(_) => {
                let fmt = Format::from_params(&params);
                return render(&fmt, Err(&ApiError::generic("could not read request body")));
            }
        }
    }

    tracing::debug!("{} c={}", parts.uri.path(), params.get("c").unwrap_or("-"));
    let fmt = Format::from_params(&params);
    let user = if authed {
        match auth::authenticate(
            &st,
            &params,
            &crate::server::middleware::rate_limit::client_key(&parts.headers),
        )
        .await
        {
            Ok(u) => Some(u),
            Err(e) => return render(&fmt, Err(&e)),
        }
    } else {
        None
    };

    let ctx = Ctx {
        st,
        params,
        user,
        headers: parts.headers,
        body: std::sync::Mutex::new(body),
    };

    match f(ctx).await {
        Ok(Reply::Data(v)) => render(&fmt, Ok(v)),
        Ok(Reply::Empty) => render(&fmt, Ok(Value::Null)),
        Ok(Reply::Raw(r)) => r,
        Err(e) => render(&fmt, Err(&e)),
    }
}

pub fn endpoint<F, Fut>(authed: bool, f: F) -> MethodRouter<AppState>
where
    F: Fn(Ctx) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = ApiResult> + Send + 'static,
{
    any(move |State(st): State<AppState>, req: Request| {
        let f = f.clone();
        async move { run(st, req, authed, f).await }
    })
}

pub fn endpoints() -> Vec<(&'static str, MethodRouter<AppState>)> {
    vec![
        ("ping", endpoint(true, system::ping)),
        ("getLicense", endpoint(true, system::get_license)),
        (
            "getOpenSubsonicExtensions",
            endpoint(false, system::extensions),
        ),
        ("tokenInfo", endpoint(true, system::token_info)),
        ("getUser", endpoint(true, users::get_user)),
        ("getUsers", endpoint(true, users::get_users)),
        ("juuzCreateApiKey", endpoint(true, users::create_api_key)),
        ("juuzLastfmStatus", endpoint(true, lastfm::status)),
        ("juuzLastfmAuthUrl", endpoint(true, lastfm::auth_url)),
        ("juuzLastfmLink", endpoint(true, lastfm::link)),
        ("juuzLastfmUnlink", endpoint(true, lastfm::unlink)),
        (
            "juuzListenbrainzStatus",
            endpoint(true, listenbrainz::status),
        ),
        ("juuzListenbrainzLink", endpoint(true, listenbrainz::link)),
        (
            "juuzListenbrainzUnlink",
            endpoint(true, listenbrainz::unlink),
        ),
        ("juuzGetApiKeys", endpoint(true, users::get_api_keys)),
        ("juuzDeleteApiKey", endpoint(true, users::delete_api_key)),
        ("getMusicFolders", endpoint(true, browse::get_music_folders)),
        ("getIndexes", endpoint(true, browse::get_indexes)),
        ("getArtists", endpoint(true, browse::get_artists)),
        ("getArtist", endpoint(true, browse::get_artist)),
        ("getAlbum", endpoint(true, browse::get_album)),
        ("getSong", endpoint(true, browse::get_song)),
        (
            "getMusicDirectory",
            endpoint(true, browse::get_music_directory),
        ),
        ("getGenres", endpoint(true, browse::get_genres)),
        ("getArtistInfo", endpoint(true, browse::get_artist_info)),
        ("getArtistInfo2", endpoint(true, browse::get_artist_info2)),
        ("getAlbumInfo", endpoint(true, browse::get_album_info)),
        ("getAlbumInfo2", endpoint(true, browse::get_album_info)),
        ("getSimilarSongs", endpoint(true, browse::get_similar_songs)),
        (
            "getSimilarSongs2",
            endpoint(true, browse::get_similar_songs2),
        ),
        ("getTopSongs", endpoint(true, browse::get_top_songs)),
        ("getAlbumList", endpoint(true, lists::get_album_list)),
        ("getAlbumList2", endpoint(true, lists::get_album_list2)),
        ("getRandomSongs", endpoint(true, lists::get_random_songs)),
        ("getSongsByGenre", endpoint(true, lists::get_songs_by_genre)),
        ("getNowPlaying", endpoint(true, lists::get_now_playing)),
        ("getStarred", endpoint(true, lists::get_starred)),
        ("getStarred2", endpoint(true, lists::get_starred2)),
        ("search2", endpoint(true, search::search2)),
        ("search3", endpoint(true, search::search3)),
        ("stream", endpoint(true, media::stream)),
        ("download", endpoint(true, media::download)),
        ("getCoverArt", endpoint(true, media::get_cover_art)),
        ("getLyrics", endpoint(true, media::get_lyrics)),
        (
            "getLyricsBySongId",
            endpoint(true, media::get_lyrics_by_song_id),
        ),
        ("star", endpoint(true, annotation::star)),
        ("unstar", endpoint(true, annotation::unstar)),
        ("setRating", endpoint(true, annotation::set_rating)),
        ("scrobble", endpoint(true, annotation::scrobble)),
        ("startScan", endpoint(true, annotation::start_scan)),
        ("getScanStatus", endpoint(true, annotation::scan_status)),
        ("juuzUploadSong", endpoint(false, upload::upload_song)),
        ("juuzDeleteSong", endpoint(true, upload::delete_song)),
        ("getPlaylists", endpoint(true, playlists::get_playlists)),
        ("getPlaylist", endpoint(true, playlists::get_playlist)),
        ("createPlaylist", endpoint(true, playlists::create_playlist)),
        ("updatePlaylist", endpoint(true, playlists::update_playlist)),
        ("deletePlaylist", endpoint(true, playlists::delete_playlist)),
        ("savePlayQueue", endpoint(true, queue::save_play_queue)),
        ("getPlayQueue", endpoint(true, queue::get_play_queue)),
        (
            "savePlayQueueByIndex",
            endpoint(true, queue::save_play_queue_by_index),
        ),
        (
            "getPlayQueueByIndex",
            endpoint(true, queue::get_play_queue_by_index),
        ),
        ("getBookmarks", endpoint(true, queue::get_bookmarks)),
        ("createBookmark", endpoint(true, queue::create_bookmark)),
        ("deleteBookmark", endpoint(true, queue::delete_bookmark)),
        ("getShares", endpoint(true, social::get_shares)),
        ("createShare", endpoint(true, social::create_share)),
        ("updateShare", endpoint(true, social::update_share)),
        ("deleteShare", endpoint(true, social::delete_share)),
        (
            "getInternetRadioStations",
            endpoint(true, social::get_radio_stations),
        ),
        (
            "createInternetRadioStation",
            endpoint(true, social::create_radio_station),
        ),
        (
            "updateInternetRadioStation",
            endpoint(true, social::update_radio_station),
        ),
        (
            "deleteInternetRadioStation",
            endpoint(true, social::delete_radio_station),
        ),
        ("search", endpoint(true, search::legacy_search)),
        ("jukeboxControl", endpoint(true, jukebox::control)),
        (
            "juuzGetJukeboxOutputs",
            endpoint(true, jukebox::get_outputs),
        ),
        (
            "juuzScanJukeboxOutputs",
            endpoint(true, jukebox::scan_outputs),
        ),
        ("juuzSetJukeboxOutput", endpoint(true, jukebox::set_output)),
        ("juuzSetJukeboxGroup", endpoint(true, jukebox::set_group)),
        (
            "juuzSetJukeboxPassword",
            endpoint(true, jukebox::set_password),
        ),
        (
            "juuzStartJukeboxPairing",
            endpoint(true, jukebox::start_pairing),
        ),
        (
            "juuzFinishJukeboxPairing",
            endpoint(true, jukebox::finish_pairing),
        ),
        (
            "juuzForgetJukeboxPairing",
            endpoint(true, jukebox::forget_pairing),
        ),
        (
            "juuzDeleteJukeboxGroup",
            endpoint(true, jukebox::delete_group),
        ),
        ("juuzJukeboxEvents", endpoint(true, jukebox::events)),
        ("juuzJukeboxMove", endpoint(true, jukebox::move_item)),
    ]
}
