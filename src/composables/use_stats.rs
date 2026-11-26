use crate::composables::use_timezone::timezone;
use crate::server::utils::data;
use crate::server::utils::data::{Playing, Range, Stats};
use crate::server::utils::db::users::User;
use crate::server::utils::session::{fail, state};
use topcoat::Result;
use topcoat::context::Cx;

#[derive(Clone)]
pub struct StatsBoard {
    pub me: User,
    pub range: Range,
    pub everyone: bool,
    pub stats: Stats,
    pub playing: Vec<Playing>,
    pub tz: i64,
}

pub async fn stats_board(cx: &Cx, me: User, range: Range, everyone: bool) -> Result<StatsBoard> {
    let st = state(cx);
    let tz = timezone(cx);
    let everyone = everyone && me.admin;
    let scope = (!everyone).then(|| me.id.clone());
    let entries: Vec<(String, String, String)> = st
        .playing_now()
        .into_iter()
        .filter(|n| scope.as_deref().is_none_or(|s| s == n.user_id))
        .map(|n| (n.song_id, n.username, n.player))
        .collect();
    let (stats, playing) = st
        .db
        .run(move |c| {
            let stats = data::stats(c, scope.as_deref(), range, tz)?;
            let playing = data::playing(c, entries);
            Ok((stats, playing))
        })
        .await
        .map_err(fail)?;
    Ok(StatsBoard {
        me,
        range,
        everyone,
        stats,
        playing,
        tz,
    })
}
