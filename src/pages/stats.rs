use crate::components::credits::credits;
use crate::components::filter_bar::Filters;
use crate::components::stats::{ranked_list, recent_list, tile};
use crate::components::ui::nav::Page;
use crate::components::ui::panel::panel;
use crate::composables::use_notice::Query;
use crate::composables::use_stats::{StatsBoard, stats_board};
use crate::layouts::shell::shell;
use crate::server::utils::data::Range;
use crate::server::utils::format::{bar, bytes, duration, wedge};
use crate::server::utils::notice::Notice;
use crate::server::utils::session::require;
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::{page, query_params};
use topcoat::view::{View, component, view};

#[page("/stats")]
pub async fn stats(cx: &Cx) -> Result<impl View> {
    let me = require(cx).await?;
    let query = query_params::<Query>(cx)?;
    let notice = Notice::parse(query.n.as_deref());
    let range = Range::parse(query.range.as_deref());
    let everyone = query.scope.as_deref() == Some("all");
    let board = stats_board(cx, me, range, everyone).await?;
    Ok(view! { stats_page(board: board, notice: notice) })
}

#[component]
pub async fn stats_page(cx: &Cx, board: StatsBoard, notice: Option<Notice>) -> Result<impl View> {
    let top = board.stats.days.iter().map(|d| d.plays).max().unwrap_or(0);
    let chart_title = format!("plays per {}", board.range.unit());
    let widths: Vec<String> = board.stats.days.iter().map(|d| bar(d.plays, top)).collect();
    let peak = board.stats.hours.iter().map(|d| d.plays).max().unwrap_or(0);
    let wedges: Vec<String> = (0..board.stats.hours.len())
        .map(|h| wedge(h, board.stats.hours[h].plays, peak))
        .collect();
    let busiest = board
        .stats
        .hours
        .iter()
        .filter(|d| d.plays == peak && peak > 0)
        .map(|d| d.label.clone())
        .next()
        .unwrap_or_else(|| "-".to_string());
    let share = |part: i64| {
        if board.stats.plays > 0 {
            format!("{}%", part * 100 / board.stats.plays)
        } else {
            "0%".to_string()
        }
    };
    let links = Page::Library.visible(&board.me);
    let completed = share(board.stats.completed);
    let skipped = share(board.stats.skipped);
    let average = if board.stats.plays > 0 {
        let secs = board.stats.listened_ms / board.stats.plays / 1000;
        format!("{}:{:02}", secs / 60, secs % 60)
    } else {
        "0:00".to_string()
    };
    let filters = Some(Filters {
        range: board.range,
        everyone: board.everyone,
        admin: board.me.admin,
    });
    Ok(view! {
        shell(
            page: Some(Page::Stats),
            name: board.me.name.as_str(),
            me: Some(board.me.clone()),
            notice: notice,
            filters: filters,
            <div class="live" data-live="stats">
                panel(
                    title: "overview",
                    note: board.range.label(),
                                        wide: true,
                                        <dl class="tiles">
                        tile(label: "plays", value: board.stats.plays.to_string())
                        tile(label: "listened", value: duration(board.stats.listened_ms))
                        tile(label: "completed", value: completed.clone())
                        tile(label: "skipped", value: skipped.clone())
                        tile(label: "avg listen", value: average.clone())
                        tile(label: "songs", value: board.stats.library.songs.to_string())
                        tile(label: "albums", value: board.stats.library.albums.to_string())
                        tile(label: "artists", value: board.stats.library.artists.to_string())
                        tile(label: "library", value: duration(board.stats.library.duration_ms))
                        tile(label: "size", value: bytes(board.stats.library.bytes))
                        tile(label: "users", value: board.stats.library.users.to_string())
                    </dl>
                )
                if !board.playing.is_empty() {
                    panel(
                        title: "now playing",
                                                wide: true,
                                                <ul>
                            for item in board.playing.iter() {
                                <li>
                                    <span class="title">(item.title.as_str()) <small>credits(list: &item.artists, off: !links)</small></span>
                                    <span class="detail">(item.user.as_str()) " · " (item.player.as_str())</span>
                                </li>
                            }
                        </ul>
                    )
                }
                panel(
                    title: chart_title.as_str(),
                    note: board.range.label(),
                    <div class="chart" role="img" aria-label=(chart_title.as_str())>
                        for (day, width) in board.stats.days.iter().zip(widths.iter()) {
                            <div class="day">
                                <span class="date">(day.label.as_str())</span>
                                <span class="bar"><i style=(width.as_str())></i></span>
                                <span class="count">(day.plays)</span>
                            </div>
                        }
                    </div>
                )
                panel(
                    title: "plays by hour",
                    note: board.range.label(),
                    <div class="dial" role="img" aria-label="plays by hour">
                        <div class="ring">
                            for (hour, shape) in board.stats.hours.iter().zip(wedges.iter()) {
                                <i style=(shape.as_str()) title=(format!("{} · {} plays", hour.label, hour.plays))></i>
                            }
                            <span class="peak"><small>"peak"</small>(busiest.as_str())</span>
                        </div>
                        <b class="mark top">"00"</b>
                        <b class="mark right">"06"</b>
                        <b class="mark bottom">"12"</b>
                        <b class="mark left">"18"</b>
                    </div>
                )
                recent_list(items: &board.stats.recent, everyone: board.everyone, tz: board.tz, links: links)
                ranked_list(title: "top songs", kind: "song", items: &board.stats.songs, links: links)
                ranked_list(title: "top albums", kind: "album", items: &board.stats.albums, links: links)
                ranked_list(title: "top artists", kind: "artist", items: &board.stats.artists, links: links)
            </div>
        )
    })
}
