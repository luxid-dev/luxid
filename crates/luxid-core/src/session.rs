//! Server-side sessions.
//!
//! A session is a bag of values keyed by an opaque id, carried in a cookie.
//! The values live in a [`SessionStore`]; the cookie carries only the id, so a
//! client can neither read nor forge the contents.
//!
//! The session handle is shared rather than owned. Middleware needs to persist
//! whatever the action did, but the action *consumes* the context — so the
//! handle wraps shared state, and the middleware keeps a clone to read back
//! afterwards. That is the same problem the owning context created for the
//! database scope, solved the same way.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::auth::Identity;
use crate::error::{Error, Result};
use crate::middleware::BoxFuture;

/// The reserved key holding the authenticated subject.
const SUBJECT: &str = "__luxid_subject";

/// Everything stored against one session id.
pub type SessionData = BTreeMap<String, Value>;

#[derive(Debug, Default)]
struct State {
    id: String,
    data: SessionData,
    dirty: bool,
    destroyed: bool,
    /// False when no session middleware ran, so writes can say so rather than
    /// vanishing.
    attached: bool,
}

/// The request's session.
#[derive(Debug, Clone, Default)]
pub struct Session {
    state: Arc<Mutex<State>>,
}

impl Session {
    /// A session that is not backed by anything. Reads are empty and writes
    /// report the missing middleware.
    pub fn detached() -> Self {
        Self::default()
    }

    pub(crate) fn attached(id: String, data: SessionData) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                id,
                data,
                dirty: false,
                destroyed: false,
                attached: true,
            })),
        }
    }

    fn with<T>(&self, read: impl FnOnce(&State) -> T) -> T {
        read(&self.state.lock().expect("session state is not poisoned"))
    }

    fn write<T>(&self, change: impl FnOnce(&mut State) -> T) -> Result<T> {
        let mut state = self.state.lock().expect("session state is not poisoned");

        if !state.attached {
            return Err(Error::internal(
                "no session is active on this route. Add `.middleware(Auth::session())`, \
                 and bind a `SessionStore` in `providers()`.",
            ));
        }

        state.dirty = true;
        Ok(change(&mut state))
    }

    pub fn id(&self) -> String {
        self.with(|state| state.id.clone())
    }

    pub fn is_active(&self) -> bool {
        self.with(|state| state.attached)
    }

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let raw = self.with(|state| state.data.get(key).cloned());

        match raw {
            Some(value) => serde_json::from_value(value)
                .map(Some)
                .map_err(|err| Error::internal(format!("session key `{key}`: {err}"))),
            None => Ok(None),
        }
    }

    pub fn put<T: Serialize>(&self, key: impl Into<String>, value: T) -> Result<()> {
        let key = key.into();
        let value = serde_json::to_value(value)
            .map_err(|err| Error::internal(format!("session key `{key}`: {err}")))?;

        self.write(|state| {
            state.data.insert(key, value);
        })
    }

    pub fn has(&self, key: &str) -> bool {
        self.with(|state| state.data.contains_key(key))
    }

    pub fn forget(&self, key: &str) -> Result<()> {
        self.write(|state| {
            state.data.remove(key);
        })
    }

    /// Empty the session, keeping the id.
    pub fn flush(&self) -> Result<()> {
        self.write(|state| state.data.clear())
    }

    /// Invalidate the session entirely: the store entry is removed and the
    /// cookie cleared.
    pub fn destroy(&self) -> Result<()> {
        self.write(|state| {
            state.data.clear();
            state.destroyed = true;
        })
    }

    /// Record who this session belongs to.
    ///
    /// Does **not** rotate the id — call [`Session::regenerate`] on login to
    /// defeat session fixation, which is what `login_as` does for you.
    pub fn set_subject(&self, subject: impl Into<String>) -> Result<()> {
        self.put(SUBJECT, subject.into())
    }

    pub fn subject(&self) -> Option<String> {
        self.with(|state| {
            state
                .data
                .get(SUBJECT)
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    }

    /// Give the session a fresh id, keeping its contents.
    ///
    /// Do this whenever privilege changes. Without it, an attacker who fixed a
    /// victim's session id before login still holds a valid id afterwards.
    pub fn regenerate(&self) -> Result<()> {
        let id = new_id();
        self.write(|state| state.id = id)
    }

    /// Log an identity in: rotate the id, then record the subject.
    pub fn login(&self, identity: &Identity) -> Result<()> {
        self.regenerate()?;
        self.set_subject(identity.subject())
    }

    /// Log out and invalidate, so the old cookie is worthless.
    pub fn logout(&self) -> Result<()> {
        self.destroy()
    }

    pub(crate) fn snapshot(&self) -> (String, SessionData, bool, bool) {
        self.with(|state| {
            (
                state.id.clone(),
                state.data.clone(),
                state.dirty,
                state.destroyed,
            )
        })
    }
}

/// Where session data lives.
pub trait SessionStore: Send + Sync + 'static {
    fn load(&self, id: &str) -> BoxFuture<'_, Result<Option<SessionData>>>;

    fn save(&self, id: &str, data: SessionData, ttl: Duration) -> BoxFuture<'_, Result<()>>;

    fn destroy(&self, id: &str) -> BoxFuture<'_, Result<()>>;
}

/// An in-process store.
///
/// Fine for a single process and for tests. Sessions are lost on restart and
/// are not shared between instances, so anything running more than one process
/// needs a shared store.
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: Mutex<BTreeMap<String, (SessionData, Instant)>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().expect("not poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SessionStore for MemoryStore {
    fn load(&self, id: &str) -> BoxFuture<'_, Result<Option<SessionData>>> {
        let id = id.to_owned();

        Box::pin(async move {
            let mut entries = self.entries.lock().expect("not poisoned");

            // Expiry is checked on read rather than swept on a timer: a store
            // this simple should not own a background task.
            match entries.get(&id) {
                Some((_, expires)) if *expires <= Instant::now() => {
                    entries.remove(&id);
                    Ok(None)
                }
                Some((data, _)) => Ok(Some(data.clone())),
                None => Ok(None),
            }
        })
    }

    fn save(&self, id: &str, data: SessionData, ttl: Duration) -> BoxFuture<'_, Result<()>> {
        let id = id.to_owned();

        Box::pin(async move {
            self.entries
                .lock()
                .expect("not poisoned")
                .insert(id, (data, Instant::now() + ttl));
            Ok(())
        })
    }

    fn destroy(&self, id: &str) -> BoxFuture<'_, Result<()>> {
        let id = id.to_owned();

        Box::pin(async move {
            self.entries.lock().expect("not poisoned").remove(&id);
            Ok(())
        })
    }
}

/// A 256-bit opaque identifier.
///
/// Sourced from the OS random generator, not a counter or a timestamp: a
/// guessable session id is an account takeover.
pub fn new_id() -> String {
    use argon2::password_hash::rand_core::RngCore;

    let mut bytes = [0u8; 32];
    argon2::password_hash::rand_core::OsRng.fill_bytes(&mut bytes);

    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::attached(new_id(), SessionData::new())
    }

    #[test]
    fn ids_are_long_and_unpredictable() {
        let a = new_id();
        let b = new_id();

        assert_eq!(a.len(), 64, "32 bytes as hex");
        assert_ne!(a, b);
    }

    #[test]
    fn values_round_trip() {
        let session = session();

        session.put("cart", vec![1, 2, 3]).expect("attached");
        session.put("name", "Ada").expect("attached");

        assert_eq!(
            session.get::<Vec<i32>>("cart").unwrap(),
            Some(vec![1, 2, 3])
        );
        assert_eq!(session.get::<String>("name").unwrap(), Some("Ada".into()));
        assert_eq!(session.get::<String>("absent").unwrap(), None);
    }

    #[test]
    fn a_detached_session_reports_the_missing_middleware() {
        let session = Session::detached();

        assert!(!session.is_active());
        assert_eq!(session.get::<String>("name").unwrap(), None);

        let err = session.put("name", "Ada").unwrap_err();
        let message = format!("{err}");

        assert!(message.contains("no session is active"), "{message}");
        assert!(message.contains("Auth::session()"), "{message}");
    }

    #[test]
    fn forget_and_flush() {
        let session = session();
        session.put("a", 1).unwrap();
        session.put("b", 2).unwrap();

        session.forget("a").unwrap();
        assert!(!session.has("a"));
        assert!(session.has("b"));

        session.flush().unwrap();
        assert!(!session.has("b"));
    }

    #[test]
    fn login_rotates_the_id_to_defeat_fixation() {
        let session = session();
        let before = session.id();

        session.login(&Identity::new("42")).expect("attached");

        assert_ne!(session.id(), before, "the id must change on login");
        assert_eq!(session.subject(), Some("42".to_owned()));
    }

    #[test]
    fn logout_destroys_the_session() {
        let session = session();
        session.login(&Identity::new("42")).unwrap();

        session.logout().unwrap();

        let (_, data, _, destroyed) = session.snapshot();
        assert!(destroyed);
        assert!(data.is_empty());
        assert_eq!(session.subject(), None);
    }

    #[test]
    fn a_clone_shares_state_with_the_original() {
        // This is what lets middleware read back what the action did.
        let session = session();
        let handle = session.clone();

        session.put("touched", true).unwrap();

        assert_eq!(handle.get::<bool>("touched").unwrap(), Some(true));
        assert!(handle.snapshot().2, "dirty must be visible to the holder");
    }

    #[tokio::test]
    async fn the_memory_store_round_trips() {
        let store = MemoryStore::new();
        let mut data = SessionData::new();
        data.insert("name".into(), Value::String("Ada".into()));

        store
            .save("abc", data.clone(), Duration::from_secs(60))
            .await
            .unwrap();

        assert_eq!(store.load("abc").await.unwrap(), Some(data));
        assert_eq!(store.load("nope").await.unwrap(), None);

        store.destroy("abc").await.unwrap();
        assert_eq!(store.load("abc").await.unwrap(), None);
    }

    #[tokio::test]
    async fn expired_entries_read_as_absent_and_are_dropped() {
        let store = MemoryStore::new();

        store
            .save("abc", SessionData::new(), Duration::from_secs(0))
            .await
            .unwrap();

        assert_eq!(store.load("abc").await.unwrap(), None);
        assert!(
            store.is_empty(),
            "the expired entry was removed, not merely hidden"
        );
    }
}
