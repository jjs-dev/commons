use serde::{Deserialize, Serialize};
use std::convert::Infallible;

pub struct AnyhowRejection(pub anyhow::Error);

impl std::fmt::Debug for AnyhowRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl warp::reject::Reject for AnyhowRejection {}

#[derive(Clone, Copy, Serialize, Deserialize, Debug)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorKind {
    /// Some parameters were invalid
    InvalidInput,
    /// Internal or unknown error
    Internal,
}

impl ErrorKind {
    pub fn http_status(self) -> http::StatusCode {
        match self {
            ErrorKind::InvalidInput => http::StatusCode::BAD_REQUEST,
            ErrorKind::Internal => http::StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Put this struct into anyhow error context to return user-facing API error
#[derive(Serialize, Deserialize, Debug)]
pub struct ApiError {
    pub kind: ErrorKind,
    pub code: String,
    pub details: serde_json::Map<String, serde_json::Value>,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        "api error".fmt(f)
    }
}

impl ApiError {
    pub fn new(kind: ErrorKind, code: &str) -> Self {
        ApiError {
            kind,
            code: code.to_string(),
            details: serde_json::Map::new(),
        }
    }

    pub fn add_detail<T: Serialize>(
        &mut self,
        key: &str,
        value: &T,
    ) -> Result<(), serde_json::Error> {
        self.details
            .insert(key.to_string(), serde_json::to_value(value)?);
        Ok(())
    }

    pub fn reply(&self) -> impl warp::Reply {
        warp::reply::with_status(warp::reply::json(&self), self.kind.http_status())
    }
}

/// Use this as a `.recover` on routes.
pub fn recover(rej: warp::Rejection) -> Result<impl warp::Reply, Infallible> {
    let api_error = rej
        .find::<AnyhowRejection>()
        .and_then(|anyhow| anyhow.0.downcast_ref::<ApiError>());
    let fallback_api_error;
    let api_error = match api_error {
        Some(err) => err,
        None => {
            tracing::warn!("unexpected error: {:?}", rej);
            fallback_api_error = ApiError::new(ErrorKind::Internal, "UnknownError");
            &fallback_api_error
        }
    };
    Ok(api_error.reply())
}
