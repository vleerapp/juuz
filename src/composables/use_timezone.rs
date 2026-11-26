use topcoat::context::Cx;
use topcoat::cookie::{Cookies, cookies};

pub fn timezone(cx: &Cx) -> i64 {
    cookies(cx)
        .get("juuz_tz")
        .and_then(|c| c.value().parse::<i64>().ok())
        .filter(|m| (-14 * 60..=14 * 60).contains(m))
        .map_or(0, |m| m * 60)
}
