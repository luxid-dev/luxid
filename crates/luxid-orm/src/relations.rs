//! Loaded relations, carried on the model itself.
//!
//! A model's `#[sea_orm(ignore)] relations: Relations` field holds whatever
//! `.with(..)` fetched. Values are stored twice: once typed, for
//! `user.posts()`, and once as JSON, so loaded relations serialize inline with
//! the model exactly as Eloquent does.
//!
//! Storing the JSON copy is the price of erasure — a `dyn Any` cannot be
//! serialized — and it keeps the model a plain `Serialize` type rather than
//! forcing every consumer through a wrapper.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use luxid_core::error::{Error, Result};
use serde::{Serialize, Serializer};
use serde_json::Value;

/// Whether reading an unloaded relation is an error.
///
/// On in debug, off in release. This is process-level rather than per-request
/// because it is a development diagnostic, not application state — the same
/// reasoning that keeps `debug_assertions` global.
static STRICT: AtomicBool = AtomicBool::new(cfg!(debug_assertions));

/// Turn unloaded-relation access into an error (the default in debug builds).
///
/// Leaving this on in tests is what turns an N+1 into a failing test rather
/// than a production surprise.
pub fn set_strict_relations(strict: bool) {
    STRICT.store(strict, Ordering::Relaxed);
}

pub fn strict_relations() -> bool {
    STRICT.load(Ordering::Relaxed)
}

#[derive(Clone)]
struct Entry {
    /// `Vec<T>` for to-many, `Option<T>` for to-one.
    typed: Arc<dyn Any + Send + Sync>,
    json: Value,
}

/// The relations loaded onto one model instance.
#[derive(Clone, Default)]
pub struct Relations {
    entries: BTreeMap<String, Entry>,
}

impl Relations {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_loaded(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    pub fn loaded_names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn insert_many<T>(&mut self, name: impl Into<String>, values: Vec<T>)
    where
        T: Any + Send + Sync + Serialize,
    {
        let json = serde_json::to_value(&values).unwrap_or(Value::Null);
        self.entries.insert(
            name.into(),
            Entry {
                typed: Arc::new(values),
                json,
            },
        );
    }

    pub fn insert_one<T>(&mut self, name: impl Into<String>, value: Option<T>)
    where
        T: Any + Send + Sync + Serialize,
    {
        let json = serde_json::to_value(&value).unwrap_or(Value::Null);
        self.entries.insert(
            name.into(),
            Entry {
                typed: Arc::new(value),
                json,
            },
        );
    }

    pub fn many<T: Any + Send + Sync>(&self, model: &str, name: &str) -> Result<&[T]> {
        match self.entries.get(name) {
            Some(entry) => entry
                .typed
                .downcast_ref::<Vec<T>>()
                .map(Vec::as_slice)
                .ok_or_else(|| mismatched(model, name)),
            None if strict_relations() => Err(not_loaded(model, name)),
            None => Ok(&[]),
        }
    }

    pub fn one<T: Any + Send + Sync>(&self, model: &str, name: &str) -> Result<Option<&T>> {
        match self.entries.get(name) {
            Some(entry) => entry
                .typed
                .downcast_ref::<Option<T>>()
                .map(Option::as_ref)
                .ok_or_else(|| mismatched(model, name)),
            None if strict_relations() => Err(not_loaded(model, name)),
            None => Ok(None),
        }
    }
}

fn not_loaded(model: &str, name: &str) -> Error {
    Error::internal(format!(
        "the `{name}` relation of `{model}` was not loaded. \
         Add `.with(\"{name}\")` to the query, or call \
         `luxid::set_strict_relations(false)` to read unloaded relations as empty."
    ))
}

fn mismatched(model: &str, name: &str) -> Error {
    Error::internal(format!(
        "the `{name}` relation of `{model}` was loaded as a different type than it was read as"
    ))
}

/// Loaded relations serialize inline, so a model with `.with(\"posts\")` renders
/// its posts alongside its own columns.
impl Serialize for Relations {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (name, entry) in &self.entries {
            map.serialize_entry(name, &entry.json)?;
        }
        map.end()
    }
}

/// Compares what is loaded, not identity: two models with the same columns and
/// the same loaded relations are equal.
impl PartialEq for Relations {
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self
                .entries
                .iter()
                .zip(other.entries.iter())
                .all(|((a_name, a), (b_name, b))| a_name == b_name && a.json == b.json)
    }
}

impl Eq for Relations {}

impl std::fmt::Debug for Relations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relations")
            .field("loaded", &self.entries.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `STRICT` is process-global, so tests that toggle it must not run
    /// concurrently with each other. Serialising them here is honest about
    /// that; making the flag per-task would be a bigger change than a
    /// development diagnostic warrants.
    static TOGGLE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[derive(Debug, Clone, PartialEq, Serialize)]
    struct Post {
        id: i64,
        title: String,
    }

    fn post(id: i64) -> Post {
        Post {
            id,
            title: format!("Post {id}"),
        }
    }

    #[test]
    fn round_trips_a_to_many_relation() {
        let mut relations = Relations::new();
        relations.insert_many("posts", vec![post(1), post(2)]);

        assert!(relations.is_loaded("posts"));
        assert_eq!(
            relations
                .many::<Post>("User", "posts")
                .expect("loaded")
                .len(),
            2
        );
    }

    #[test]
    fn round_trips_a_to_one_relation() {
        let mut relations = Relations::new();
        relations.insert_one("author", Some(post(9)));

        assert_eq!(
            relations.one::<Post>("Comment", "author").expect("loaded"),
            Some(&post(9))
        );
    }

    #[test]
    fn an_unloaded_relation_errors_under_strict_mode() {
        let _guard = TOGGLE.lock().expect("not poisoned");

        set_strict_relations(true);
        let relations = Relations::new();

        let err = relations.many::<Post>("User", "posts").unwrap_err();
        let message = format!("{err}");

        assert!(message.contains("was not loaded"), "{message}");
        assert!(message.contains(".with(\"posts\")"), "{message}");
    }

    #[test]
    fn an_unloaded_relation_reads_as_empty_when_not_strict() {
        let _guard = TOGGLE.lock().expect("not poisoned");

        set_strict_relations(false);
        let relations = Relations::new();

        assert!(
            relations
                .many::<Post>("User", "posts")
                .expect("lenient")
                .is_empty()
        );
        assert_eq!(
            relations.one::<Post>("User", "team").expect("lenient"),
            None
        );

        set_strict_relations(true);
    }

    #[test]
    fn loaded_relations_serialize_inline() {
        let mut relations = Relations::new();
        relations.insert_many("posts", vec![post(1)]);
        relations.insert_one("team", Some(post(3)));

        let json = serde_json::to_value(&relations).expect("serializes");

        assert_eq!(json["posts"][0]["id"], 1);
        assert_eq!(json["team"]["id"], 3);
    }

    #[test]
    fn an_empty_bag_serializes_to_an_empty_map() {
        let json = serde_json::to_value(Relations::new()).expect("serializes");
        assert_eq!(json, serde_json::json!({}));
    }
}
