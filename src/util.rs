use serde_json::{Value, json};

pub fn iso(ts: i64) -> String {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn date_obj(raw: &str) -> Option<Value> {
    let mut parts = raw.trim().split(['-', '/', '.']);
    let year: i64 = parts.next()?.get(..4)?.parse().ok()?;
    let mut obj = json!({"year": year});
    if let Some(month) = parts.next().and_then(|m| m.get(..2)?.parse::<i64>().ok())
        && (1..=12).contains(&month)
    {
        obj["month"] = json!(month);
        if let Some(day) = parts.next().and_then(|d| d.get(..2)?.parse::<i64>().ok())
            && (1..=31).contains(&day)
        {
            obj["day"] = json!(day);
        }
    }
    Some(obj)
}

pub fn placeholders(n: usize) -> String {
    std::iter::repeat_n("?", n).collect::<Vec<_>>().join(",")
}

pub fn strip_article<'a>(name: &'a str, articles: &[&str]) -> &'a str {
    let lower = name.to_lowercase();
    for a in articles {
        if let Some(rest) = lower.strip_prefix(&a.to_lowercase())
            && rest.starts_with(' ')
        {
            return name[a.len()..].trim_start();
        }
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_formats_epoch() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_700_000_000), "2023-11-14T22:13:20.000Z");
    }

    #[test]
    fn date_parts() {
        assert_eq!(
            date_obj("2019-10-18"),
            Some(json!({"year": 2019, "month": 10, "day": 18}))
        );
        assert_eq!(date_obj("2019"), Some(json!({"year": 2019})));
        assert_eq!(date_obj("x"), None);
    }

    #[test]
    fn articles() {
        assert_eq!(strip_article("The Beatles", &["The", "A"]), "Beatles");
        assert_eq!(strip_article("Theory", &["The"]), "Theory");
    }
}
