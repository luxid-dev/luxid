//! Form-request validation.
//!
//! Rules come in two kinds. Synchronous rules — length, format, range — run
//! against the value alone. Asynchronous rules — `unique`, `exists` — need the
//! database.
//!
//! This module knows nothing about databases, and it does not need to: async
//! rules run through the request's *ambient* connection, the same task-local
//! the data layer uses. So the trait takes no database argument, and
//! `luxid-core` keeps no dependency on `luxid-orm`.

use crate::error::{Error, Result, ValidationErrors};
use crate::middleware::BoxFuture;

/// Implemented by `#[derive(Validate)]`.
pub trait Validate {
    /// Rules that need only the value.
    fn validate_sync(&self) -> ValidationErrors;

    /// Rules that need the database.
    ///
    /// `skip` names fields that already failed synchronously — there is no
    /// point asking the database whether a malformed email is taken, and doing
    /// so would report two errors for one mistake.
    fn validate_async<'a>(
        &'a self,
        skip: &'a [&'a str],
    ) -> BoxFuture<'a, Result<ValidationErrors>> {
        let _ = skip;
        Box::pin(async { Ok(ValidationErrors::new()) })
    }
}

/// Run every rule and aggregate the failures into a single 422.
///
/// Async rules for independent fields run in one batch, so a form with three
/// database-backed rules costs one round of queries, not three requests.
pub async fn validate<T: Validate>(value: &T) -> Result<()> {
    let sync = value.validate_sync();
    let failed: Vec<&str> = sync.field_names().collect();

    let mut errors = sync.clone();
    errors.merge(value.validate_async(&failed).await?);

    if errors.is_empty() {
        return Ok(());
    }

    Err(Error::Validation(errors))
}

/// Helpers the derive expands into. Not a stable API.
#[doc(hidden)]
pub mod rules {
    /// A pragmatic email check, not RFC 5322.
    ///
    /// Full compliance accepts addresses no mail system will deliver to and
    /// rejects nothing users actually type; every framework that tries ends up
    /// with a regex nobody can read. This checks the shape people mean.
    pub fn is_email(value: &str) -> bool {
        let Some((local, domain)) = value.split_once('@') else {
            return false;
        };

        !local.is_empty()
            && !domain.is_empty()
            && !value.contains(char::is_whitespace)
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.')
            && !domain.contains("..")
    }

    /// Characters, not bytes: "café" is four characters, and a user counting
    /// them agrees.
    pub fn length(value: &str) -> usize {
        value.chars().count()
    }

    pub fn too_short(min: usize) -> String {
        format!("must be at least {min} character{}", plural(min))
    }

    pub fn too_long(max: usize) -> String {
        format!("must be at most {max} character{}", plural(max))
    }

    pub fn wrong_length(expected: usize) -> String {
        format!("must be exactly {expected} character{}", plural(expected))
    }

    fn plural(count: usize) -> &'static str {
        if count == 1 { "" } else { "s" }
    }
}

#[cfg(test)]
mod tests {
    use super::rules::*;

    #[test]
    fn accepts_ordinary_addresses() {
        assert!(is_email("ada@example.com"));
        assert!(is_email("ada+tag@mail.example.co.uk"));
    }

    #[test]
    fn rejects_malformed_addresses() {
        for value in [
            "",
            "ada",
            "ada@",
            "@example.com",
            "ada@example",
            "a b@example.com",
            "ada@.com",
            "ada@example.",
            "ada@ex..com",
        ] {
            assert!(!is_email(value), "{value} should be rejected");
        }
    }

    #[test]
    fn counts_characters_not_bytes() {
        assert_eq!(length("café"), 4);
        assert_eq!(length("日本語"), 3);
    }

    #[test]
    fn messages_agree_with_the_number() {
        assert_eq!(too_short(1), "must be at least 1 character");
        assert_eq!(too_short(8), "must be at least 8 characters");
        assert_eq!(too_long(1), "must be at most 1 character");
        assert_eq!(wrong_length(3), "must be exactly 3 characters");
    }
}
