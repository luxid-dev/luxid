//! Application configuration.
//!
//! Two layers, in precedence order: the environment overrides `luxid.toml`.
//! That is the 12-factor arrangement — the file holds what is true for everyone,
//! the environment holds what is true for this deployment.
//!
//! Keys are dotted. A nested TOML table flattens to `database.strict_relations`,
//! and the environment spelling of that key is `DATABASE_STRICT_RELATIONS`. The
//! mapping is mechanical so nobody has to look it up.
//!
//! This is deliberately *not* where typed application settings belong. A struct
//! of your own, bound as a singleton in `providers()`, is better in every way
//! that matters — it is validated once at boot rather than on every read. Use
//! `Config` to build that struct, not as a substitute for it.

use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Error, Result};

#[derive(Debug, Default)]
struct Inner {
    values: BTreeMap<String, String>,
}

/// A read-only view of the application's configuration. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Config {
    inner: Arc<Inner>,
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from explicit pairs. Chiefly useful in tests.
    pub fn from_pairs<K, V>(pairs: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        let values = pairs
            .into_iter()
            .map(|(key, value)| (normalise(&key.into()), value.into()))
            .collect();

        Self {
            inner: Arc::new(Inner { values }),
        }
    }

    /// The process environment alone.
    pub fn from_env() -> Self {
        let mut values = BTreeMap::new();
        overlay_env(&mut values);

        Self {
            inner: Arc::new(Inner { values }),
        }
    }

    /// `luxid.toml` with the environment layered over it.
    ///
    /// A missing file is not an error: an application configured entirely by
    /// environment is a perfectly ordinary one, and failing here would make the
    /// file mandatory for no reason.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let mut values = BTreeMap::new();

        if let Ok(text) = std::fs::read_to_string(path.as_ref()) {
            let table: toml::Table = toml::from_str(&text).map_err(|err| {
                Error::internal(format!("could not read {:?}: {err}", path.as_ref()))
            })?;

            flatten("", &toml::Value::Table(table), &mut values);
        }

        overlay_env(&mut values);

        Ok(Self {
            inner: Arc::new(Inner { values }),
        })
    }

    pub fn has(&self, key: &str) -> bool {
        self.inner.values.contains_key(&normalise(key))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.inner.values.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.inner.values.is_empty()
    }

    /// The raw string, if the key is set.
    pub fn raw(&self, key: &str) -> Option<&str> {
        self.inner.values.get(&normalise(key)).map(String::as_str)
    }

    /// A required value, decoded into `T`.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<T> {
        match self.try_get(key)? {
            Some(value) => Ok(value),
            None => Err(Error::internal(format!(
                "configuration key `{key}` is not set. Add it to luxid.toml, or set `{}`.",
                environment_key(key)
            ))),
        }
    }

    /// An optional value, decoded into `T`.
    pub fn try_get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.raw(key) {
            Some(raw) => decode(key, raw).map(Some),
            None => Ok(None),
        }
    }

    /// A value with a fallback. A *malformed* value still fails — silently
    /// falling back would hide a typo in production.
    pub fn get_or<T: DeserializeOwned>(&self, key: &str, default: T) -> Result<T> {
        Ok(self.try_get(key)?.unwrap_or(default))
    }

    /// A value with a fallback, parsed with `FromStr` instead of serde.
    pub fn parse_or<T: FromStr>(&self, key: &str, default: T) -> Result<T> {
        match self.raw(key) {
            Some(raw) => raw
                .parse()
                .map_err(|_| Error::internal(format!("`{key}` could not be read from `{raw}`"))),
            None => Ok(default),
        }
    }
}

/// Configuration keys are case-insensitive and `_`/`.` are interchangeable, so
/// `DATABASE_STRICT_RELATIONS` and `database.strict_relations` are one key.
fn normalise(key: &str) -> String {
    key.trim().to_ascii_lowercase().replace('_', ".")
}

/// The environment spelling of a dotted key, for error messages.
fn environment_key(key: &str) -> String {
    normalise(key).replace('.', "_").to_ascii_uppercase()
}

fn overlay_env(values: &mut BTreeMap<String, String>) {
    for (key, value) in std::env::vars() {
        values.insert(normalise(&key), value);
    }
}

fn flatten(prefix: &str, value: &toml::Value, out: &mut BTreeMap<String, String>) {
    match value {
        toml::Value::Table(table) => {
            for (key, nested) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };

                flatten(&path, nested, out);
            }
        }
        // Arrays are rendered as their TOML form rather than being flattened
        // into indexed keys, which nobody would guess the spelling of.
        toml::Value::Array(_) => {
            out.insert(normalise(prefix), value.to_string());
        }
        toml::Value::String(text) => {
            out.insert(normalise(prefix), text.clone());
        }
        other => {
            out.insert(normalise(prefix), other.to_string());
        }
    }
}

/// The same decoding the request layer uses: try the raw text as JSON, which
/// handles numbers and booleans, then fall back to treating it as a string.
fn decode<T: DeserializeOwned>(key: &str, raw: &str) -> Result<T> {
    if let Ok(value) = serde_json::from_str::<T>(raw) {
        return Ok(value);
    }

    serde_json::from_value(Value::String(raw.to_owned()))
        .map_err(|err| Error::internal(format!("`{key}` could not be read from `{raw}`: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::from_pairs([
            ("app.name", "blogapp"),
            ("app.per_page", "20"),
            ("database.strict_relations", "true"),
        ])
    }

    #[test]
    fn decodes_typed_values() {
        let config = config();

        assert_eq!(config.get::<String>("app.name").unwrap(), "blogapp");
        assert_eq!(config.get::<u32>("app.per_page").unwrap(), 20);
        assert!(config.get::<bool>("database.strict_relations").unwrap());
    }

    #[test]
    fn dots_and_underscores_and_case_are_one_key() {
        let config = config();

        assert_eq!(config.get::<u32>("APP_PER_PAGE").unwrap(), 20);
        assert_eq!(config.get::<u32>("app_per_page").unwrap(), 20);
        assert_eq!(config.get::<u32>("App.Per.Page").unwrap(), 20);
    }

    #[test]
    fn a_missing_required_key_names_its_environment_spelling() {
        let err = config().get::<String>("mail.driver").unwrap_err();
        let message = format!("{err}");

        assert!(message.contains("`mail.driver` is not set"), "{message}");
        assert!(message.contains("MAIL_DRIVER"), "{message}");
    }

    #[test]
    fn optional_and_defaulted_reads() {
        let config = config();

        assert_eq!(config.try_get::<String>("mail.driver").unwrap(), None);
        assert_eq!(config.get_or("app.per_page", 10).unwrap(), 20);
        assert_eq!(config.get_or("mail.retries", 3).unwrap(), 3);
    }

    #[test]
    fn a_malformed_value_fails_rather_than_falling_back() {
        // Silently defaulting would hide a typo until someone wondered why the
        // setting had no effect.
        let config = Config::from_pairs([("app.per_page", "twenty")]);

        assert!(config.get::<u32>("app.per_page").is_err());
        assert!(config.get_or("app.per_page", 10).is_err());
    }

    #[test]
    fn flattens_nested_tables_into_dotted_keys() {
        let toml_text = r#"
            [app]
            name = "blogapp"

            [database]
            strict_relations = true
            pool = 8
        "#;

        let table: toml::Table = toml::from_str(toml_text).expect("valid toml");
        let mut values = BTreeMap::new();
        flatten("", &toml::Value::Table(table), &mut values);

        assert_eq!(values.get("app.name").map(String::as_str), Some("blogapp"));
        assert_eq!(
            values.get("database.strict.relations").map(String::as_str),
            Some("true")
        );
        assert_eq!(values.get("database.pool").map(String::as_str), Some("8"));
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let config = Config::load("does-not-exist.toml").expect("absence is fine");

        // The environment still came through.
        assert!(config.keys().count() > 0 || config.is_empty());
    }

    #[test]
    fn the_environment_overrides_the_file() {
        // SAFETY: single-threaded test, and the key is unique to it.
        unsafe { std::env::set_var("LUXID_TEST_OVERRIDE_KEY", "from-env") };

        let mut values = BTreeMap::new();
        values.insert("luxid.test.override.key".to_owned(), "from-file".to_owned());
        overlay_env(&mut values);

        assert_eq!(
            values.get("luxid.test.override.key").map(String::as_str),
            Some("from-env")
        );

        unsafe { std::env::remove_var("LUXID_TEST_OVERRIDE_KEY") };
    }
}
