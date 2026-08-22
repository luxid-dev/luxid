//! The single error type every Luxid action can `?` on.
//!
//! Each variant carries its own HTTP mapping, which is what allows an action
//! body to stay free of error-handling ceremony: `User::find_or_fail(id).await?`
//! becomes a clean 404 without a line of translation.

use std::collections::BTreeMap;

use salvo::http::StatusCode;
use serde_json::{Value, json};

pub type Result<T> = std::result::Result<T, Error>;

/// Field-keyed validation failures, aggregated so a request reports every
/// problem at once rather than one per round trip.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ValidationErrors(BTreeMap<String, Vec<String>>);

impl ValidationErrors {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, field: impl Into<String>, message: impl Into<String>) {
        self.0.entry(field.into()).or_default().push(message.into());
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn fields(&self) -> impl Iterator<Item = (&String, &Vec<String>)> {
        self.0.iter()
    }

    pub fn field_names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    pub fn has(&self, field: &str) -> bool {
        self.0.contains_key(field)
    }

    /// Fold another set in, keeping both sides' messages for shared fields.
    pub fn merge(&mut self, other: Self) {
        for (field, messages) in other.0 {
            self.0.entry(field).or_default().extend(messages);
        }
    }

    fn to_json(&self) -> Value {
        json!(self.0)
    }
}

impl FromIterator<(String, Vec<String>)> for ValidationErrors {
    fn from_iter<I: IntoIterator<Item = (String, Vec<String>)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the given data was invalid")]
    Validation(ValidationErrors),

    #[error("{resource} `{id}` not found")]
    NotFound { resource: &'static str, id: String },

    #[error("unauthenticated")]
    Unauthorized,

    #[error("this action is forbidden")]
    Forbidden,

    #[error("{0}")]
    Conflict(String),

    #[error("too many requests")]
    TooManyRequests,

    #[error("{0}")]
    BadRequest(String),

    #[error("{message}")]
    Http {
        status: u16,
        code: String,
        message: String,
        details: Option<Value>,
    },

    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl Error {
    pub fn not_found(resource: &'static str, id: impl std::fmt::Display) -> Self {
        Self::NotFound {
            resource,
            id: id.to_string(),
        }
    }

    /// Raise a 500 without needing `anyhow` in scope. The message is logged
    /// in full and never reaches the client.
    pub fn internal(message: impl std::fmt::Display) -> Self {
        Self::Internal(anyhow::anyhow!("{message}"))
    }

    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::NotFound { .. } => StatusCode::NOT_FOUND,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Http { status, .. } => {
                StatusCode::from_u16(*status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }

    /// Stable, machine-readable slug. Also forms the `type` URI in the
    /// RFC 7807 payload, so it is part of the public API surface.
    pub fn code(&self) -> &str {
        match self {
            Self::Validation(_) => "validation",
            Self::NotFound { .. } => "not-found",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::Conflict(_) => "conflict",
            Self::TooManyRequests => "too-many-requests",
            Self::BadRequest(_) => "bad-request",
            Self::Internal(_) => "internal",
            Self::Http { code, .. } => code,
        }
    }

    /// Whether the message is safe to show a client. Internal and database
    /// failures are logged in full but never echoed back.
    fn is_public(&self) -> bool {
        !matches!(self, Self::Internal(_))
    }

    /// RFC 7807 `application/problem+json` body.
    pub fn problem(&self) -> Value {
        let mut problem = json!({
            "type": format!("https://luxid.rs/errors/{}", self.code()),
            "title": if self.is_public() { self.to_string() } else { "internal server error".to_owned() },
            "status": self.status_code().as_u16(),
        });

        let map = problem.as_object_mut().expect("problem is an object");

        match self {
            Self::Validation(errors) => {
                map.insert("errors".into(), errors.to_json());
            }
            Self::NotFound { resource, id } => {
                map.insert("resource".into(), json!(resource));
                map.insert("id".into(), json!(id));
            }
            Self::Http {
                details: Some(details),
                ..
            } => {
                map.insert("details".into(), details.clone());
            }
            _ => {}
        }

        problem
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Self::BadRequest(format!("malformed JSON: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_variants_to_status_codes() {
        assert_eq!(Error::Unauthorized.status_code(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            Error::not_found("User", 7).status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            Error::Validation(ValidationErrors::new()).status_code(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn validation_problem_carries_field_errors() {
        let mut errors = ValidationErrors::new();
        errors.add("email", "must be a valid email address");
        errors.add("email", "is already taken");
        errors.add("name", "is required");

        let problem = Error::Validation(errors).problem();

        assert_eq!(problem["status"], 422);
        assert_eq!(problem["type"], "https://luxid.rs/errors/validation");
        assert_eq!(problem["errors"]["email"][1], "is already taken");
        assert_eq!(problem["errors"]["name"][0], "is required");
    }

    #[test]
    fn the_internal_constructor_redacts_too() {
        let problem = Error::internal("secret token abc123").problem();

        assert_eq!(problem["status"], 500);
        assert!(!problem.to_string().contains("abc123"));
    }

    #[test]
    fn internal_errors_are_redacted() {
        let err = Error::Internal(anyhow::anyhow!(
            "connection string: postgres://user:hunter2@db"
        ));
        let problem = err.problem();

        assert_eq!(problem["status"], 500);
        assert_eq!(problem["title"], "internal server error");
        assert!(!problem.to_string().contains("hunter2"));
    }

    #[test]
    fn not_found_reports_the_resource() {
        let problem = Error::not_found("User", 42).problem();
        assert_eq!(problem["status"], 404);
        assert_eq!(problem["resource"], "User");
        assert_eq!(problem["id"], "42");
    }
}
