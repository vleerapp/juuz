mod actions;
mod auth;
mod data;
mod live;
mod notice;
mod page;
mod sketch;

use std::time::Duration;

use topcoat::cookie::{Cookie, Cookies, RouterBuilderCookieExt, SameSite, cookies};
use topcoat::router::{Router, tower::TowerService};
use topcoat::session::{
    RouterBuilderSessionExt, SessionConfig, Token, TokenStore, TokenStoreFuture,
};

use crate::state::AppState;

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
                    .same_site(SameSite::Lax)
                    .max_age(max_age)
                    .build(),
            );
            Ok(())
        })
    }

    fn delete<'a>(&'a self, cx: &'a topcoat::context::Cx) -> TokenStoreFuture<'a, ()> {
        Box::pin(async move {
            cookies(cx).remove(Cookie::build((COOKIE, "")).path("/").build());
            Ok(())
        })
    }
}

pub const PATHS: [&str; 12] = [
    "/",
    "/login",
    "/logout",
    "/juuz.css",
    "/paper.webp",
    "/dareo.otf",
    "/live.js",
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
    let router = builder
        .cookies()
        .sessions(
            SessionConfig::builder()
                .token_store(CookieStore)
                .lifetime(LIFETIME)
                .build(),
        )
        .app_context(st)
        .build();
    TowerService::new(router)
}
