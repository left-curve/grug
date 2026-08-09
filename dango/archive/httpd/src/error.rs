//! The read API's error type — the bridge from a feed `Result` to an actix HTTP
//! response, shared by every projection's read handlers.
//!
//! A malformed cursor or argument is the caller's fault ([`ApiError::bad_request`]
//! → 400); everything else — a database error, a corrupt stored payload, a block
//! that won't decode — is ours (500). The body is always a small
//! `{"error": "…"}` JSON object so clients can parse failures uniformly.

use {
    actix_web::{HttpResponse, ResponseError, http::StatusCode},
    std::fmt,
};

/// An error surfaced by a read-API feed handler.
#[derive(Debug)]
pub enum ApiError {
    /// The request is malformed — a bad cursor, an unparseable argument. 400.
    BadRequest(String),
    /// Something went wrong server-side — a query failed, a stored payload did
    /// not decode, a block could not be read. 500.
    Internal(String),
}

impl ApiError {
    /// A 400 from a caller-facing message.
    pub fn bad_request<M>(message: M) -> Self
    where
        M: fmt::Display,
    {
        Self::BadRequest(message.to_string())
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadRequest(message) | Self::Internal(message) => f.write_str(message),
        }
    }
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_response(&self) -> HttpResponse {
        HttpResponse::build(self.status_code())
            .json(serde_json::json!({ "error": self.to_string() }))
    }
}

// ---- conversions: everything not explicitly a bad request is internal ----

impl From<sea_orm::DbErr> for ApiError {
    fn from(err: sea_orm::DbErr) -> Self {
        Self::Internal(format!("database error: {err}"))
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(err.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        Self::Internal(format!("serialization error: {err}"))
    }
}
