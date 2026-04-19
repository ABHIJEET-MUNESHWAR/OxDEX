//! API error mapping (`OxDexError` → HTTP status + JSON body).

use actix_web::{http::StatusCode, HttpResponse, ResponseError};
use serde::Serialize;
use std::fmt;

use oxdex_types::OxDexError;

/// Wire shape returned for every error.
#[derive(Debug, Serialize)]
pub struct ApiErrorBody {
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Human-readable description.
    pub message: String,
}

/// Newtype wrapper so we can `impl ResponseError`.
#[derive(Debug)]
pub struct ApiError(pub OxDexError);

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<OxDexError> for ApiError {
    fn from(e: OxDexError) -> Self {
        Self(e)
    }
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        match &self.0 {
            OxDexError::InvalidAddress(_)
            | OxDexError::InvalidOrder(_)
            | OxDexError::BadSignature(_)
            | OxDexError::InvalidSolution(_) => StatusCode::BAD_REQUEST,
            OxDexError::NotFound(_) => StatusCode::NOT_FOUND,
            OxDexError::Conflict(_) => StatusCode::CONFLICT,
            OxDexError::Storage(_) | OxDexError::Network(_) => StatusCode::SERVICE_UNAVAILABLE,
            OxDexError::Config(_) | OxDexError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_response(&self) -> HttpResponse {
        let body = ApiErrorBody {
            code: self.0.code(),
            message: self.0.to_string(),
        };
        HttpResponse::build(self.status_code()).json(body)
    }
}
