use std::path::Path;

use anyhow::{Context, Result, anyhow};
use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use md5::Md5;
use sha2::{Digest, Sha256};

use crate::db::users::{self, User};
use crate::error::ApiError;
use crate::params::Params;
use crate::state::AppState;

pub struct Crypto {
    key: [u8; 32],
}

impl Crypto {
    pub fn load_or_create(path: &Path) -> Result<Crypto> {
        if path.exists() {
            let bytes = std::fs::read(path).context("reading secret key")?;
            let key: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow!("secret key file must be exactly 32 bytes"))?;
            return Ok(Crypto { key });
        }
        let key: [u8; 32] = rand::random();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, key).context("writing secret key")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Crypto { key })
    }

    pub fn seal(&self, plain: &[u8]) -> Vec<u8> {
        let nonce_bytes: [u8; 12] = rand::random();
        let cipher = ChaCha20Poly1305::new(&Key::from(self.key));
        let ct = cipher
            .encrypt(&Nonce::from(nonce_bytes), plain)
            .expect("encryption failure");
        let mut out = nonce_bytes.to_vec();
        out.extend(ct);
        out
    }

    pub fn sign(&self, data: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.key);
        hasher.update(b"juuz-sign");
        hasher.update(data.as_bytes());
        hex::encode(hasher.finalize())
    }

    pub fn open(&self, data: &[u8]) -> Option<Vec<u8>> {
        if data.len() < 13 {
            return None;
        }
        let (nonce, ct) = data.split_at(12);
        let cipher = ChaCha20Poly1305::new(&Key::from(self.key));
        let nonce = Nonce::try_from(nonce).ok()?;
        cipher.decrypt(&nonce, ct).ok()
    }
}

pub fn hash_password(password: &str) -> Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| anyhow!("hashing password: {e}"))
}

pub async fn credentials(crypto: &Crypto, password: &str) -> Result<(String, Vec<u8>), ApiError> {
    let sealed = crypto.seal(password.as_bytes());
    let password = password.to_string();
    let hash = tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|_| ApiError::generic("internal server error"))??;
    Ok((hash, sealed))
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

pub fn hash_api_key(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

pub fn generate_api_key() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut key = String::from("jz_");
    for _ in 0..40 {
        let i = rand::random::<u8>() as usize % ALPHABET.len();
        key.push(ALPHABET[i] as char);
    }
    key
}

pub fn token_for(password: &str, salt: &str) -> String {
    let mut h = Md5::new();
    h.update(password.as_bytes());
    h.update(salt.as_bytes());
    hex::encode(h.finalize())
}

pub fn constant_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn decode_password(raw: &str) -> Result<String, ApiError> {
    match raw.strip_prefix("enc:") {
        Some(h) => {
            let bytes = hex::decode(h).map_err(|_| ApiError::bad_param("p"))?;
            String::from_utf8(bytes).map_err(|_| ApiError::bad_param("p"))
        }
        None => Ok(raw.to_string()),
    }
}

pub async fn authenticate(st: &AppState, p: &Params) -> Result<User, ApiError> {
    if let Some(key) = p.get("apiKey") {
        if p.has("u") || p.has("p") || p.has("t") || p.has("s") {
            return Err(ApiError::conflicting_auth());
        }
        let hash = hash_api_key(key);
        let user = st.db.run(move |c| Ok(users::by_api_key(c, &hash)?)).await?;
        return user.ok_or_else(ApiError::invalid_api_key);
    }

    let name = p.req("u")?.to_string();
    let user = {
        let name = name.clone();
        st.db.run(move |c| Ok(users::by_name(c, &name)?)).await?
    }
    .ok_or_else(ApiError::wrong_credentials)?;

    let secret = user
        .token_secret
        .as_deref()
        .and_then(|s| st.crypto.open(s))
        .and_then(|b| String::from_utf8(b).ok());

    if let Some(raw) = p.get("p") {
        let password = decode_password(raw)?;
        if let Some(stored) = secret {
            return if constant_eq(&stored, &password) {
                Ok(user)
            } else {
                Err(ApiError::wrong_credentials())
            };
        }
        let hash = user.password_hash.clone();
        let candidate = password.clone();
        let ok = tokio::task::spawn_blocking(move || verify_password(&hash, &candidate))
            .await
            .unwrap_or(false);
        if !ok {
            return Err(ApiError::wrong_credentials());
        }
        let sealed = st.crypto.seal(password.as_bytes());
        let id = user.id.clone();
        st.db
            .run(move |c| Ok(users::set_token_secret(c, &id, &sealed)?))
            .await?;
        return Ok(user);
    }

    if let (Some(token), Some(salt)) = (p.get("t"), p.get("s")) {
        let Some(password) = secret else {
            return Err(ApiError::new(
                41,
                "log in once with a password so token authentication can be enabled",
            ));
        };
        return if constant_eq(
            &token_for(&password, salt).to_lowercase(),
            &token.to_lowercase(),
        ) {
            Ok(user)
        } else {
            Err(ApiError::wrong_credentials())
        };
    }

    Err(ApiError::missing("p"))
}
