use crate::error::ApiError;

#[derive(Clone, Default, Debug)]
pub struct Params(pub Vec<(String, String)>);

impl Params {
    pub fn parse(raw: &str) -> Params {
        Params(
            form_urlencoded::parse(raw.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect(),
        )
    }

    pub fn extend(&mut self, other: Params) {
        self.0.extend(other.0);
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn all(&self, key: &str) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    pub fn has(&self, key: &str) -> bool {
        self.0.iter().any(|(k, _)| k == key)
    }

    pub fn req(&self, key: &str) -> Result<&str, ApiError> {
        self.get(key)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| ApiError::missing(key))
    }

    pub fn int(&self, key: &str) -> Result<Option<i64>, ApiError> {
        match self.get(key).filter(|v| !v.is_empty()) {
            None => Ok(None),
            Some(v) => v
                .trim()
                .parse::<i64>()
                .map(Some)
                .map_err(|_| ApiError::bad_param(key)),
        }
    }

    pub fn int_or(&self, key: &str, default: i64) -> Result<i64, ApiError> {
        Ok(self.int(key)?.unwrap_or(default))
    }

    pub fn ints(&self, key: &str) -> Result<Vec<i64>, ApiError> {
        self.all(key)
            .into_iter()
            .map(|v| {
                v.trim()
                    .parse::<i64>()
                    .map_err(|_| ApiError::bad_param(key))
            })
            .collect()
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.get(key)
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
    }
}
