use axum::{
    http::{HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{message}")]
    RateLimited {
        message: String,
        headers: Vec<(String, String)>,
    },
    #[error("{0}")]
    Backend(String),
    #[error("{0}")]
    Internal(String),
}

#[derive(Debug, Serialize)]
struct OpenAiErrorEnvelope {
    error: OpenAiError,
}

#[derive(Debug, Serialize)]
struct OpenAiError {
    message: String,
    #[serde(rename = "type")]
    error_type: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::BadRequest(message) => {
                make_error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message)
            }
            AppError::Unauthorized(message) => {
                make_error_response(StatusCode::UNAUTHORIZED, "authentication_error", message)
            }
            AppError::RateLimited { message, headers } => {
                let mut response =
                    make_error_response(StatusCode::TOO_MANY_REQUESTS, "rate_limit_error", message);
                for (name, value) in headers {
                    apply_header(response.headers_mut(), &name, &value);
                }
                response
            }
            AppError::Backend(message) => {
                make_error_response(StatusCode::BAD_GATEWAY, "backend_error", message)
            }
            AppError::Internal(message) => {
                make_error_response(StatusCode::INTERNAL_SERVER_ERROR, "server_error", message)
            }
        }
    }
}

fn make_error_response(status: StatusCode, error_type: &str, message: String) -> Response {
    let payload = OpenAiErrorEnvelope {
        error: OpenAiError {
            message,
            error_type: error_type.to_owned(),
        },
    };

    (status, Json(payload)).into_response()
}

pub fn apply_header(headers: &mut axum::http::HeaderMap, name: &str, value: &str) {
    let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) else {
        return;
    };
    let Ok(header_value) = HeaderValue::from_str(value) else {
        return;
    };
    headers.insert(header_name, header_value);
}
