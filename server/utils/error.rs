use std::fmt;

#[derive(Debug, Clone)]
pub struct ApiError {
    pub code: u32,
    pub message: String,
}

impl ApiError {
    pub fn new(code: u32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn generic(message: impl Into<String>) -> Self {
        Self::new(0, message)
    }

    pub fn missing(param: &str) -> Self {
        Self::new(10, format!("required parameter is missing: {param}"))
    }

    pub fn bad_param(param: &str) -> Self {
        Self::new(10, format!("invalid parameter: {param}"))
    }

    pub fn wrong_credentials() -> Self {
        Self::new(40, "wrong username or password")
    }

    pub fn conflicting_auth() -> Self {
        Self::new(
            43,
            "multiple conflicting authentication mechanisms provided",
        )
    }

    pub fn invalid_api_key() -> Self {
        Self::new(44, "invalid api key")
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(50, message)
    }

    pub fn not_found(what: &str) -> Self {
        Self::new(70, format!("{what} not found"))
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for ApiError {}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        match e.downcast::<ApiError>() {
            Ok(api) => api,
            Err(e) => {
                tracing::error!("internal error: {e:#}");
                ApiError::generic("internal server error")
            }
        }
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        tracing::error!("database error: {e}");
        ApiError::generic("database error")
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        tracing::error!("io error: {e}");
        ApiError::generic("io error")
    }
}
