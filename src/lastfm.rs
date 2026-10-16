use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use anyhow::{Result, anyhow};
use md5::{Digest, Md5};
use serde_json::Value;

use crate::config::LastfmConfig;

pub const MIN_SCROBBLE_SECS: u32 = 30;
pub const INVALID_SESSION: i64 = 9;

#[derive(Debug)]
pub struct ApiFailure {
    pub code: i64,
    pub message: String,
}

impl fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "last.fm error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiFailure {}

pub struct Track<'a> {
    pub artist: &'a str,
    pub title: &'a str,
    pub album: Option<&'a str>,
    pub duration_secs: u32,
}

pub struct Client {
    agent: ureq::Agent,
    key: String,
    secret: String,
    api_url: String,
    auth_url: String,
}

impl Client {
    pub fn new(cfg: &LastfmConfig) -> Option<Client> {
        let (key, secret) = (cfg.api_key.clone()?, cfg.secret.clone()?);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .user_agent(concat!("juuz/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Some(Client {
            agent,
            key,
            secret,
            api_url: cfg.api_url.clone(),
            auth_url: cfg.auth_url.clone(),
        })
    }

    pub fn sign(&self, params: &BTreeMap<&str, String>) -> String {
        let mut text = String::new();
        for (k, v) in params {
            text.push_str(k);
            text.push_str(v);
        }
        text.push_str(&self.secret);
        hex::encode(Md5::digest(text.as_bytes()))
    }

    fn call(&self, method: &str, extra: &[(&'static str, String)], post: bool) -> Result<Value> {
        let mut params: BTreeMap<&str, String> = BTreeMap::new();
        params.insert("api_key", self.key.clone());
        params.insert("method", method.to_string());
        for (k, v) in extra {
            params.insert(k, v.clone());
        }
        let sig = self.sign(&params);

        let mut resp = if post {
            let mut form: Vec<(String, String)> = params
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect();
            form.push(("api_sig".into(), sig));
            form.push(("format".into(), "json".into()));
            self.agent.post(&self.api_url).send_form(form)?
        } else {
            let mut req = self.agent.get(&self.api_url).query("method", method);
            for (k, v) in &params {
                if *k != "method" {
                    req = req.query(*k, v.as_str());
                }
            }
            req.query("api_sig", sig).query("format", "json").call()?
        };
        let status = resp.status().as_u16();
        let body: Value = resp
            .body_mut()
            .read_json()
            .map_err(|e| anyhow!("last.fm returned an unreadable response (HTTP {status}): {e}"))?;
        if let Some(code) = body.get("error").and_then(Value::as_i64) {
            return Err(ApiFailure {
                code,
                message: body["message"].as_str().unwrap_or("").to_string(),
            }
            .into());
        }
        if status >= 400 {
            return Err(anyhow!("last.fm returned HTTP {status}"));
        }
        Ok(body)
    }

    pub fn auth_token(&self) -> Result<String> {
        let body = self.call("auth.getToken", &[], false)?;
        body["token"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("last.fm did not return a token"))
    }

    pub fn auth_link(&self, token: &str) -> String {
        format!("{}?api_key={}&token={token}", self.auth_url, self.key)
    }

    pub fn web_auth_link(&self, callback: &str) -> String {
        let cb: String = form_urlencoded::byte_serialize(callback.as_bytes()).collect();
        format!("{}?api_key={}&cb={cb}", self.auth_url, self.key)
    }

    pub fn session(&self, token: &str) -> Result<(String, String)> {
        let body = self.call("auth.getSession", &[("token", token.to_string())], false)?;
        let key = body["session"]["key"].as_str();
        let name = body["session"]["name"].as_str();
        match (key, name) {
            (Some(k), Some(n)) => Ok((k.to_string(), n.to_string())),
            _ => Err(anyhow!("last.fm did not return a session")),
        }
    }

    fn track_params(session: &str, t: &Track) -> Vec<(&'static str, String)> {
        let mut p = vec![
            ("sk", session.to_string()),
            ("artist", t.artist.to_string()),
            ("track", t.title.to_string()),
        ];
        if let Some(album) = t.album {
            p.push(("album", album.to_string()));
        }
        if t.duration_secs > 0 {
            p.push(("duration", t.duration_secs.to_string()));
        }
        p
    }

    pub fn now_playing(&self, session: &str, t: &Track) -> Result<()> {
        self.call(
            "track.updateNowPlaying",
            &Self::track_params(session, t),
            true,
        )?;
        Ok(())
    }

    pub fn scrobble(&self, session: &str, t: &Track, timestamp: i64) -> Result<()> {
        let mut p = Self::track_params(session, t);
        p.push(("timestamp", timestamp.to_string()));
        self.call("track.scrobble", &p, true)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_follows_the_documented_algorithm() {
        let client = Client::new(&LastfmConfig {
            api_key: Some("key".into()),
            secret: Some("secret".into()),
            ..Default::default()
        })
        .unwrap();
        let mut params = BTreeMap::new();
        params.insert("method", "auth.getSession".to_string());
        params.insert("api_key", "key".to_string());
        params.insert("token", "tok".to_string());
        let expected = hex::encode(Md5::digest(
            b"api_keykeymethodauth.getSessiontokentoksecret",
        ));
        assert_eq!(client.sign(&params), expected);
    }

    #[test]
    fn disabled_without_both_values() {
        assert!(Client::new(&LastfmConfig::default()).is_none());
        let only_key = LastfmConfig {
            api_key: Some("k".into()),
            ..Default::default()
        };
        assert!(Client::new(&only_key).is_none());
    }
}
