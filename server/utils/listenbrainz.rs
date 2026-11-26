use std::fmt;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

pub const DEFAULT_URL: &str = "https://api.listenbrainz.org";
pub const MIN_SCROBBLE_SECS: u32 = 30;

#[derive(Debug)]
pub struct Rejected {
    pub status: u16,
    pub message: String,
}

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "listenbrainz error {}: {}", self.status, self.message)
    }
}

impl std::error::Error for Rejected {}

impl Rejected {
    pub fn is_auth(&self) -> bool {
        self.status == 401
    }
}

pub struct Track<'a> {
    pub artist: &'a str,
    pub title: &'a str,
    pub album: Option<&'a str>,
    pub duration_secs: u32,
}

pub struct Client {
    agent: ureq::Agent,
    base: String,
    token: String,
}

pub fn normalize_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let url = if trimmed.is_empty() {
        DEFAULT_URL
    } else {
        trimmed
    };
    let url = url.trim_end_matches('/');
    let url = url.strip_suffix("/1/submit-listens").unwrap_or(url);
    let url = url.strip_suffix("/1/validate-token").unwrap_or(url);
    let url = url.strip_suffix("/1").unwrap_or(url);
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    if rest.is_empty() || rest.starts_with('/') || rest.contains(char::is_whitespace) {
        return None;
    }
    Some(url.to_string())
}

pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest)
}

impl Client {
    pub fn new(base: &str, token: &str) -> Client {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .user_agent(concat!("juuz/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Client {
            agent,
            base: base.to_string(),
            token: token.to_string(),
        }
    }

    fn auth(&self) -> String {
        format!("Token {}", self.token)
    }

    fn failure(status: u16, body: &Value) -> Rejected {
        Rejected {
            status,
            message: body["error"]
                .as_str()
                .or_else(|| body["message"].as_str())
                .unwrap_or("")
                .to_string(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        let mut resp = self
            .agent
            .get(&format!("{}/1/validate-token", self.base))
            .header("Authorization", &self.auth())
            .call()?;
        let status = resp.status().as_u16();
        let body: Value = resp.body_mut().read_json().map_err(|e| {
            anyhow!("listenbrainz returned an unreadable response (HTTP {status}): {e}")
        })?;
        if status >= 400 {
            return Err(Self::failure(status, &body).into());
        }
        if body["valid"].as_bool() != Some(true) {
            return Err(Rejected {
                status: 401,
                message: "invalid token".into(),
            }
            .into());
        }
        Ok(())
    }

    fn submit(&self, listen_type: &str, payload: Vec<Value>) -> Result<()> {
        let body = json!({"listen_type": listen_type, "payload": payload});
        let mut resp = self
            .agent
            .post(&format!("{}/1/submit-listens", self.base))
            .header("Authorization", &self.auth())
            .send_json(body)?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let body: Value = resp.body_mut().read_json().unwrap_or(Value::Null);
            return Err(Self::failure(status, &body).into());
        }
        Ok(())
    }

    fn metadata(t: &Track) -> Value {
        let mut info = json!({
            "media_player": "juuz",
            "media_player_version": env!("CARGO_PKG_VERSION"),
            "submission_client": "juuz",
            "submission_client_version": env!("CARGO_PKG_VERSION"),
        });
        if t.duration_secs > 0 {
            info["duration"] = json!(t.duration_secs);
        }
        let mut meta = json!({
            "artist_name": t.artist,
            "track_name": t.title,
            "additional_info": info,
        });
        if let Some(album) = t.album {
            meta["release_name"] = json!(album);
        }
        meta
    }

    pub fn now_playing(&self, t: &Track) -> Result<()> {
        self.submit(
            "playing_now",
            vec![json!({"track_metadata": Self::metadata(t)})],
        )
    }

    pub fn scrobble(&self, listens: &[(Track, i64)]) -> Result<()> {
        let payload: Vec<Value> = listens
            .iter()
            .map(|(t, at)| json!({"listened_at": at, "track_metadata": Self::metadata(t)}))
            .collect();
        let kind = if payload.len() == 1 {
            "single"
        } else {
            "import"
        };
        self.submit(kind, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_normalized() {
        assert_eq!(normalize_url("").as_deref(), Some(DEFAULT_URL));
        assert_eq!(
            normalize_url(" http://koito:4110/apis/listenbrainz/ ").as_deref(),
            Some("http://koito:4110/apis/listenbrainz")
        );
        assert_eq!(
            normalize_url("http://maloja:42010/apis/listenbrainz/1/submit-listens").as_deref(),
            Some("http://maloja:42010/apis/listenbrainz")
        );
        assert_eq!(
            normalize_url("https://api.listenbrainz.org/1").as_deref(),
            Some("https://api.listenbrainz.org")
        );
        assert!(normalize_url("ftp://x").is_none());
        assert!(normalize_url("https://").is_none());
        assert!(normalize_url("javascript:alert(1)").is_none());
    }

    #[test]
    fn host_strips_scheme_and_path() {
        assert_eq!(host("http://koito:4110/apis/listenbrainz"), "koito:4110");
    }
}
