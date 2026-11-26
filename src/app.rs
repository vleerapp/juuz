use std::time::Duration;

use topcoat::asset::{AssetBundle, RouterBuilderAssetExt};
use topcoat::cookie::{Cookie, CookieLayer, Cookies, SameSite, cookies};
use topcoat::router::{Router, tower::TowerService};
use topcoat::runtime::RuntimeLayer;
use topcoat::session::{SessionLifetime, SessionTokenStore, Token, TokenStore, TokenStoreFuture};

use crate::components::ui::icon::sheet;
use crate::composables::use_runtime::runtime_on;
use crate::server::utils::session::https;
use crate::server::utils::state::AppState;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::{HeaderName, HeaderValue, Slot, header, layout, route};
use topcoat::view::{View, view};

pub const COOKIE: &str = "juuz_session";
const LIFETIME: Duration = Duration::from_secs(60 * 60 * 24 * 30);

struct CookieStore;

impl TokenStore for CookieStore {
    fn read<'a>(&'a self, cx: &'a topcoat::context::Cx) -> TokenStoreFuture<'a, Option<Token>> {
        Box::pin(async move {
            Ok(cookies(cx)
                .get(COOKIE)
                .and_then(|c| Token::decode(c.value_trimmed()).ok()))
        })
    }

    fn write<'a>(
        &'a self,
        cx: &'a topcoat::context::Cx,
        token: Token,
        max_age: Duration,
    ) -> TokenStoreFuture<'a, ()> {
        Box::pin(async move {
            let max_age = topcoat::cookie::time::Duration::try_from(max_age)?;
            cookies(cx).add(
                Cookie::build((COOKIE, token.encode()))
                    .path("/")
                    .http_only(true)
                    .secure(https(cx))
                    .same_site(SameSite::Lax)
                    .max_age(max_age)
                    .build(),
            );
            Ok(())
        })
    }

    fn delete<'a>(&'a self, cx: &'a topcoat::context::Cx) -> TokenStoreFuture<'a, ()> {
        Box::pin(async move {
            cookies(cx).remove(
                Cookie::build((COOKIE, ""))
                    .path("/")
                    .secure(https(cx))
                    .build(),
            );
            Ok(())
        })
    }
}

pub const PATHS: [&str; 20] = [
    "/",
    "/login",
    "/logout",
    "/juuz.css",
    "/departure-mono.woff2",
    "/logo.svg",
    "/favicon.ico",
    "/client.js",
    "/_topcoat/{*rest}",
    "/events",
    "/account/{*rest}",
    "/users/{*rest}",
    "/server/{*rest}",
    "/account",
    "/users",
    "/server",
    "/player.js",
    "/library",
    "/stats",
    "/library/{*rest}",
];

pub fn service(st: AppState) -> TowerService {
    use crate::pages::library::candidates;
    use crate::server::api::{self, account, library, server, users};
    let builder = Router::builder()
        .route(api::login_post::login)
        .route(api::logout_post::logout)
        .route(api::events_get::events)
        .route(account::profile_post::profile)
        .route(account::password_post::password)
        .route(account::apikeys::delete_post::delete_key)
        .route(account::lastfm::start_post::lastfm_start)
        .route(account::lastfm::callback_get::lastfm_callback)
        .route(account::lastfm::unlink_post::lastfm_unlink)
        .route(account::listenbrainz::link_post::listenbrainz_link)
        .route(account::listenbrainz::unlink_post::listenbrainz_unlink)
        .route(users::create_post::create_user)
        .route(users::update_post::update_user)
        .route(users::libraries_post::user_libraries)
        .route(users::delete_post::delete_user)
        .route(server::scan_post::scan)
        .route(server::metadata::refetch_post::refetch_metadata)
        .layout(frame)
        .page(crate::pages::index::home)
        .page(crate::pages::stats::stats)
        .page(crate::pages::account::account_page)
        .page(crate::pages::users::users_page)
        .page(crate::pages::server::server_page)
        .page(crate::pages::account::create_key)
        .route(stylesheet)
        .route(font)
        .route(logo)
        .route(favicon)
        .route(client_script)
        .route(server::libraries::create_post::create_library)
        .route(server::libraries::update_post::update_library)
        .route(server::libraries::scan_post::scan_library)
        .route(server::libraries::delete_post::delete_library)
        .route(server::libraries::missing_post::remove_missing)
        .route(crate::pages::library::index::legacy)
        .page(candidates::index::candidates_page)
        .page(candidates::album::candidates_album_page)
        .page(candidates::artist::candidates_artist_page)
        .route(library::pick::album_post::pick_album)
        .route(library::pick::artist_post::pick_artist)
        .route(library::clear::album_post::clear_album)
        .route(library::clear::artist_post::clear_artist)
        .route(library::pick::index_post::pick)
        .route(library::clear::index_post::clear)
        .route(script)
        .route(library::playlists::create_post::create)
        .route(library::playlists::add_post::add)
        .route(library::playlists::move_post::reorder)
        .route(library::playlists::shift_post::shifted)
        .route(library::playlists::rename_post::rename)
        .route(library::playlists::remove_post::remove)
        .route(library::playlists::delete_post::delete);
    let builder = builder
        .layer(CookieLayer::new())
        .with(SessionTokenStore::new(CookieStore))
        .with(SessionLifetime(LIFETIME))
        .with(st);
    let builder = match AssetBundle::load() {
        Ok(bundle) => builder.assets(bundle).layer(RuntimeLayer::new()),
        Err(e) => {
            tracing::warn!("topcoat asset bundle not found, client-side navigation is off: {e}");
            builder
        }
    };
    TowerService::new(builder.build())
}
pub const STYLE: &str = concat!(
    include_str!("../styles/base.css"),
    include_str!("../styles/layout.css"),
    include_str!("../styles/components.css"),
    include_str!("../styles/library.css"),
    include_str!("../styles/player.css"),
    include_str!("../styles/rows.css"),
    include_str!("../styles/editing.css"),
    include_str!("../styles/mobile.css"),
);

pub const FONT: &[u8] = include_bytes!("../public/fonts/DepartureMono.woff2");

pub const LOGO: &str = include_str!("../public/logo.svg");

pub const FAVICON: &[u8] = include_bytes!("../public/favicon.ico");

pub const CLIENT: &str = include_str!(concat!(env!("OUT_DIR"), "/js/client.js"));

#[route(GET "/juuz.css")]
pub async fn stylesheet(cx: &Cx) -> Result<([(HeaderName, HeaderValue); 2], &'static str)> {
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/css; charset=utf-8"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=86400"),
            ),
        ],
        STYLE,
    ))
}

#[route(GET "/departure-mono.woff2")]
pub async fn font(cx: &Cx) -> Result<([(HeaderName, HeaderValue); 2], &'static [u8])> {
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("font/woff2")),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=604800"),
            ),
        ],
        FONT,
    ))
}

#[route(GET "/logo.svg")]
pub async fn logo(cx: &Cx) -> Result<([(HeaderName, HeaderValue); 2], &'static str)> {
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("image/svg+xml"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=604800"),
            ),
        ],
        LOGO,
    ))
}

#[route(GET "/favicon.ico")]
pub async fn favicon(cx: &Cx) -> Result<([(HeaderName, HeaderValue); 2], &'static [u8])> {
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("image/x-icon"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            ),
        ],
        FAVICON,
    ))
}

#[route(GET "/client.js")]
pub async fn client_script(cx: &Cx) -> Result<([(HeaderName, HeaderValue); 2], &'static str)> {
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/javascript; charset=utf-8"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=86400"),
            ),
        ],
        CLIENT,
    ))
}

pub fn style_version() -> u64 {
    xxhash_rust::xxh3::xxh3_64(STYLE.as_bytes())
}

pub fn logo_version() -> u64 {
    xxhash_rust::xxh3::xxh3_64(LOGO.as_bytes())
}

pub fn client_version() -> u64 {
    xxhash_rust::xxh3::xxh3_64(CLIENT.as_bytes())
}

#[layout("/")]
pub async fn frame(cx: &Cx, slot: Slot<'_>) -> Result<impl View> {
    Ok(view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <meta name="color-scheme" content="dark">
                <title>"Juuz"</title>
                <link rel="icon" href="/favicon.ico" sizes="any">
                <link rel="preload" href="/departure-mono.woff2" as="font" type="font/woff2" crossorigin="">
                <link rel="stylesheet" href=(format!("/juuz.css?v={:x}", style_version()))>
                <script src=(format!("/client.js?v={:x}", client_version()))></script>
                <script src=(format!("/player.js?v={:x}", script_version())) defer=""></script>
                if runtime_on(cx) { topcoat::runtime::script() }
            </head>
            <body>
                (slot)
                sheet()
            </body>
        </html>
        ((header::CACHE_CONTROL, HeaderValue::from_static("no-store")))
    })
}

pub const SCRIPT: &str = include_str!(concat!(env!("OUT_DIR"), "/js/player.js"));

pub fn script_version() -> u64 {
    xxhash_rust::xxh3::xxh3_64(SCRIPT.as_bytes())
}

#[route(GET "/player.js")]
pub async fn script(cx: &Cx) -> Result<([(HeaderName, HeaderValue); 2], &'static str)> {
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/javascript; charset=utf-8"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=86400"),
            ),
        ],
        SCRIPT,
    ))
}
