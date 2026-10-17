mod actions;
mod auth;
mod data;
mod live;
mod nav;
mod notice;
mod page;

use std::time::Duration;

use topcoat::asset::{AssetBundle, RouterBuilderAssetExt};
use topcoat::cookie::{Cookie, Cookies, RouterBuilderCookieExt, SameSite, cookies};
use topcoat::router::{Router, tower::TowerService};
use topcoat::runtime::RouterBuilderRuntimeExt;
use topcoat::session::{
    RouterBuilderSessionExt, SessionConfig, Token, TokenStore, TokenStoreFuture,
};

use crate::state::AppState;
use auth::https;

const COOKIE: &str = "juuz_session";
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

pub const PATHS: [&str; 14] = [
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
    "/admin/{*rest}",
    "/account",
    "/admin",
];

pub fn service(st: AppState) -> TowerService {
    let builder = page::routes(actions::routes(live::routes(auth::routes(
        Router::builder(),
    ))));
    let builder = builder
        .cookies()
        .sessions(
            SessionConfig::builder()
                .token_store(CookieStore)
                .lifetime(LIFETIME)
                .build(),
        )
        .app_context(st);
    let builder = match AssetBundle::load() {
        Ok(bundle) => builder.assets(bundle).runtime(),
        Err(e) => {
            tracing::warn!("topcoat asset bundle not found, client-side navigation is off: {e}");
            builder
        }
    };
    TowerService::new(builder.build())
}
