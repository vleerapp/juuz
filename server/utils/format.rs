use crate::server::utils::data;

pub fn duration(ms: i64) -> String {
    let mins = ms / 60_000;
    match (mins / 1440, (mins / 60) % 24, mins % 60) {
        (0, 0, 0) => format!("{}s", ms / 1000),
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

pub fn bytes(n: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn ago(ts: i64, tz: i64) -> String {
    let secs = (crate::server::utils::db::now() - ts).max(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => data::date_label(ts, tz),
    }
}

pub fn bar(value: i64, max: i64) -> String {
    if value <= 0 || max <= 0 {
        return "width: 0".to_string();
    }
    format!("width: {:.3}%", value as f64 * 100.0 / max as f64)
}

pub fn wedge(hour: usize, plays: i64, peak: i64) -> String {
    use std::f64::consts::TAU;
    const HOLE: f64 = 0.46;
    const STUB: f64 = 0.05;
    const GAP: f64 = 0.018;
    const STEPS: usize = 8;
    let frac = if peak > 0 {
        plays as f64 / peak as f64
    } else {
        0.0
    };
    let outer = HOLE + STUB + (1.0 - HOLE - STUB) * frac;
    let width = TAU / 24.0;
    let start = hour as f64 * width + GAP - TAU / 4.0;
    let end = (hour + 1) as f64 * width - GAP - TAU / 4.0;
    let point = |radius: f64, angle: f64| {
        format!(
            "{:.2}% {:.2}%",
            50.0 + radius * 50.0 * angle.cos(),
            50.0 + radius * 50.0 * angle.sin()
        )
    };
    let mut points: Vec<String> = (0..=STEPS)
        .map(|i| point(outer, start + (end - start) * i as f64 / STEPS as f64))
        .collect();
    points.extend(
        (0..=STEPS)
            .rev()
            .map(|i| point(HOLE, start + (end - start) * i as f64 / STEPS as f64)),
    );
    let fade = if plays > 0 { "" } else { "; opacity: 0.25" };
    format!("clip-path: polygon({}){}", points.join(", "), fade)
}

pub fn clock(ms: i64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

pub fn escape(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
