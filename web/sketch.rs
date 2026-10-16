use std::fmt::Write;
use std::ops::Range;

const VARIANTS: usize = 6;
const COLOR_VARIANTS: usize = 3;
const TEXT: &str = "%231a1a1a";
const DANGER: &str = "%23650a00";
const OK: &str = "%23004d1f";
const BLUE: &str = "%23001861";

const BLUE_BOX_OPACITY: f64 = 0.9;
const CHECK_OPACITY: f64 = 1.0;
const SCOPE: &str =
    ".card, label, .row > *, .form > *, .chip, .notice, .secret, .col, .tile, .members > *, hr";

type Point = (f64, f64);

fn wobble(amp: f64) -> f64 {
    rand::random_range(-amp..amp)
}

fn smooth(p: &[Point]) -> String {
    let mid = |a: Point, b: Point| ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    let first = mid(p[0], p[1]);
    let mut d = format!(
        "M{:.1},{:.1} L{:.1},{:.1}",
        p[0].0, p[0].1, first.0, first.1
    );
    for i in 1..p.len() - 1 {
        let to = mid(p[i], p[i + 1]);
        let _ = write!(d, " Q{:.1},{:.1} {:.1},{:.1}", p[i].0, p[i].1, to.0, to.1);
    }
    let last = p[p.len() - 1];
    let _ = write!(d, " L{:.1},{:.1}", last.0, last.1);
    d
}

fn stroke(from: Point, to: Point, overshoot: f64, amp: f64, shift: f64, count: usize) -> String {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let len = dx.hypot(dy);
    let (ux, uy) = (dx / len, dy / len);
    let (nx, ny) = (-uy, ux);
    let points: Vec<Point> = (0..=count)
        .map(|i| {
            let t = i as f64 / count as f64;
            let push = match i {
                0 => -rand::random_range(0.0..=overshoot),
                n if n == count => len + rand::random_range(0.0..=overshoot),
                _ => len * (t + wobble(0.5 / count as f64)),
            };
            let side = shift + wobble(amp);
            (
                from.0 + ux * push + nx * side,
                from.1 + uy * push + ny * side,
            )
        })
        .collect();
    smooth(&points)
}

fn svg(body: &str, width: u32, height: u32, extra: &str) -> String {
    format!(
        "url(\"data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='{width}' height='{height}' viewBox='0 0 {width} {height}'{extra}>{body}</svg>\")"
    )
}

fn path(d: &str, color: &str, width: f64, opacity: f64) -> String {
    format!(
        "<path d='{d}' fill='none' stroke='{color}' stroke-width='{width:.2}' stroke-linecap='round' opacity='{opacity}'/>"
    )
}

fn outline(color: &str, opacity: f64) -> String {
    let corners = [(6.0, 6.0), (194.0, 6.0), (194.0, 194.0), (6.0, 194.0)];
    let mut body = String::new();
    for i in 0..4 {
        let (a, b) = (corners[i], corners[(i + 1) % 4]);
        body += &path(
            &stroke(a, b, 5.0, 2.6, 0.0, rand::random_range(3..6)),
            color,
            rand::random_range(1.4..2.6),
            opacity,
        );
    }
    svg(&body, 200, 200, "")
}

const FILL_WIDTH: Range<f64> = 1.6..2.3;
const FILL_OPACITY: Range<f64> = 0.5..0.72;
const BAR_WIDTH: Range<f64> = 2.4..3.2;
const BAR_OPACITY: Range<f64> = 0.82..0.96;

fn hatch(color: &str, widths: Range<f64>, opacities: Range<f64>) -> String {
    const TILE: f64 = 96.0;
    const LINES: u32 = 28;
    const REACH: f64 = 2.0;
    let step = TILE / f64::from(LINES);
    let mut body = String::new();
    for k in 1..=LINES {
        let c = f64::from(k) * step;
        let width = rand::random_range(widths.clone());
        let opacity = rand::random_range(opacities.clone());
        let bow = wobble(0.9) / std::f64::consts::SQRT_2;
        let mut pieces = vec![((c + REACH, -REACH), (-REACH, c + REACH))];
        if k < LINES {
            pieces.push(((TILE + REACH, c - REACH), (c - REACH, TILE + REACH)));
        }
        for (from, to) in pieces {
            let control = ((from.0 + to.0) / 2.0 + bow, (from.1 + to.1) / 2.0 + bow);
            let _ = write!(
                body,
                "<path d='M{:.1},{:.1} Q{:.1},{:.1} {:.1},{:.1}' fill='none' stroke='{color}' stroke-width='{width:.2}' opacity='{opacity:.2}'/>",
                from.0, from.1, control.0, control.1, to.0, to.1
            );
        }
    }
    svg(&body, 96, 96, "")
}

fn rule(color: &str, opacity: f64) -> String {
    let d = stroke(
        (0.0, 8.0),
        (200.0, 8.0),
        0.0,
        4.4,
        0.0,
        rand::random_range(7..12),
    );
    let body = stroke_path(&d, color, rand::random_range(1.4..2.2), opacity);
    svg(&body, 200, 16, " preserveAspectRatio='none'")
}

fn stroke_path(d: &str, color: &str, width: f64, opacity: f64) -> String {
    format!(
        "<path d='{d}' fill='none' stroke='{color}' stroke-width='{width:.2}' opacity='{opacity}' stroke-linecap='round' vector-effect='non-scaling-stroke'/>"
    )
}

fn check(color: &str, opacity: f64) -> String {
    let jitter = |x: f64, y: f64, a: f64| (x + wobble(a), y + wobble(a));
    let points = [
        jitter(4.0, 12.5, 1.6),
        jitter(7.5, 15.5, 1.2),
        jitter(10.0, 19.5, 1.2),
        jitter(15.0, 11.0, 1.6),
        jitter(22.0, 3.5, 1.6),
    ];
    let body = stroke_path(
        &smooth(&points),
        color,
        rand::random_range(2.6..3.4),
        opacity,
    );
    svg(&body, 26, 24, " preserveAspectRatio='none'")
}

fn spread(count: usize) -> Vec<f64> {
    let mut at: Vec<f64> = (0..count).map(|_| rand::random_range(10.0..90.0)).collect();
    at.sort_by(f64::total_cmp);
    at
}

fn rag() -> String {
    let px = || rand::random_range(0.0..4.5);
    let mut pts = vec![format!("{:.1}px {:.1}px", px(), px())];
    for at in spread(rand::random_range(2..5)) {
        pts.push(format!("{at:.0}% {:.1}px", px()));
    }
    pts.push(format!("calc(100% - {:.1}px) {:.1}px", px(), px()));
    for at in spread(rand::random_range(1..3)) {
        pts.push(format!("calc(100% - {:.1}px) {at:.0}%", px()));
    }
    pts.push(format!(
        "calc(100% - {:.1}px) calc(100% - {:.1}px)",
        px(),
        px()
    ));
    for at in spread(rand::random_range(2..5)).into_iter().rev() {
        pts.push(format!("{at:.0}% calc(100% - {:.1}px)", px()));
    }
    pts.push(format!("{:.1}px calc(100% - {:.1}px)", px(), px()));
    for at in spread(rand::random_range(1..3)).into_iter().rev() {
        pts.push(format!("{:.1}px {at:.0}%", px()));
    }
    format!("polygon({})", pts.join(", "))
}

fn bar() -> String {
    let top = || rand::random_range(0..18);
    let low = || rand::random_range(90..101);
    format!(
        "polygon(0 {}%, {}% {}%, {}% {}%, 100% {}%, {}% {}%, {}% {}%)",
        top(),
        rand::random_range(15..38),
        top(),
        rand::random_range(52..78),
        top(),
        top(),
        rand::random_range(92..101),
        low(),
        rand::random_range(0..8),
        low()
    )
}

pub fn css() -> String {
    let mut out = String::from(":root {\n");
    for i in 0..VARIANTS {
        let _ = writeln!(out, "  --box-text-{i}: {};", outline(TEXT, 1.0));
        let _ = writeln!(
            out,
            "  --box-blue-{i}: {};",
            outline(BLUE, BLUE_BOX_OPACITY)
        );
        let _ = writeln!(
            out,
            "  --hatch-bar-{i}: {};",
            hatch(BLUE, BAR_WIDTH, BAR_OPACITY)
        );
        let _ = writeln!(
            out,
            "  --hatch-{i}: {};",
            hatch(BLUE, FILL_WIDTH, FILL_OPACITY)
        );
        let _ = writeln!(out, "  --rule-{i}: {};", rule(TEXT, 1.0));
        let _ = writeln!(out, "  --check-{i}: {};", check(BLUE, CHECK_OPACITY));
        let _ = writeln!(out, "  --bar-{i}: {};", bar());
        let _ = writeln!(out, "  --rag-{i}: {};", rag());
    }
    for i in 0..COLOR_VARIANTS {
        let _ = writeln!(out, "  --box-danger-{i}: {};", outline(DANGER, 1.0));
        let _ = writeln!(out, "  --box-ok-{i}: {};", outline(OK, 1.0));
    }
    out.push_str(&pick(0));
    out.push_str("}\n");
    for (nth, i) in [
        ("6n + 1", 1),
        ("6n + 2", 2),
        ("6n + 3", 3),
        ("6n + 4", 4),
        ("6n + 5", 5),
        ("6n", 0),
    ] {
        let _ = writeln!(out, ":is({SCOPE}):nth-child({nth}) {{\n{}}}", pick(i));
    }
    out
}

fn pick(i: usize) -> String {
    let c = i % COLOR_VARIANTS;
    [
        ("box-text", i),
        ("box-danger", c),
        ("box-ok", c),
        ("box-blue", i),
        ("hatch", i),
        ("hatch-bar", i),
        ("rule", i),
        ("check", i),
        ("bar", i),
        ("rag", i),
    ]
    .iter()
    .map(|(name, n)| format!("  --{name}: var(--{name}-{n});\n"))
    .collect()
}
