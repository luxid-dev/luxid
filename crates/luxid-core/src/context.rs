//! `HttpContext` — the single argument every Luxid action receives.

use std::any::{Any, TypeId};
use std::collections::HashMap;

use serde::de::DeserializeOwned;

use crate::auth::Auth;
use crate::config::Config;
use crate::container::Container;
use crate::error::{Error, Result};
use crate::http::{Request, Response};
use crate::session::Session;

/// Route parameters captured from the path (`/users/{id}`).
#[derive(Debug, Default, Clone)]
pub struct Params(HashMap<String, String>);

impl Params {
    pub(crate) fn new(inner: HashMap<String, String>) -> Self {
        Self(inner)
    }

    pub fn raw(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// A parameter the route declares. Absence means the route pattern and the
    /// action disagree, which is a bug in the app rather than bad input.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<T> {
        let raw = self
            .raw(key)
            .ok_or_else(|| Error::BadRequest(format!("route parameter `{key}` is missing")))?;

        crate::http::decode_param(key, raw)
    }

    pub fn try_get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.raw(key) {
            Some(raw) => crate::http::decode_param(key, raw).map(Some),
            None => Ok(None),
        }
    }
}

/// A typed, request-scoped bag.
///
/// This is the mechanism middleware uses to hand values to downstream
/// middleware and to the action — and the substrate the `auth`, `db` and
/// `services` fields will be built on. It works precisely because the context
/// is owned and threaded through one chain rather than rebuilt per step.
#[derive(Default)]
pub struct Extensions(HashMap<TypeId, Box<dyn Any + Send + Sync>>);

impl Extensions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a value, returning any previous value of the same type.
    pub fn insert<T: Any + Send + Sync>(&mut self, value: T) -> Option<T> {
        self.0
            .insert(TypeId::of::<T>(), Box::new(value))
            .and_then(|previous| previous.downcast().ok().map(|boxed| *boxed))
    }

    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.0
            .get(&TypeId::of::<T>())
            .and_then(|value| value.downcast_ref())
    }

    pub fn get_mut<T: Any + Send + Sync>(&mut self) -> Option<&mut T> {
        self.0
            .get_mut(&TypeId::of::<T>())
            .and_then(|value| value.downcast_mut())
    }

    pub fn take<T: Any + Send + Sync>(&mut self) -> Option<T> {
        self.0
            .remove(&TypeId::of::<T>())
            .and_then(|value| value.downcast().ok().map(|boxed| *boxed))
    }

    pub fn contains<T: Any + Send + Sync>(&self) -> bool {
        self.0.contains_key(&TypeId::of::<T>())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Extensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Extensions")
            .field("entries", &self.0.len())
            .finish()
    }
}

/// Everything an action needs, owned.
///
/// `#[non_exhaustive]` is load-bearing: it forces downstream crates to write
/// `..` in struct patterns (E0638), which makes adding fields — `auth`, `db`,
/// `inertia` — permanently non-breaking.
#[non_exhaustive]
#[derive(Debug)]
pub struct HttpContext {
    pub request: Request,
    pub response: Response,
    pub params: Params,
    pub extensions: Extensions,
    /// Request-scoped view of the application's services.
    pub services: Container,
    /// Who the request is. Anonymous unless a guard set it.
    pub auth: Auth,
    /// Merged configuration: `luxid.toml` with the environment over it.
    pub config: Config,
    /// The request's session. Detached unless `Auth::session()` is active.
    pub session: Session,
    /// Inertia protocol state. Detached unless the `Inertia` middleware ran.
    pub inertia: crate::inertia::InertiaRequest,
}

impl HttpContext {
    /// Enforce a policy, or fail with a 403.
    ///
    /// A policy is any function of `(&Auth, &T) -> bool`, so it is an ordinary
    /// function in an ordinary `impl` — no trait to implement, no registry to
    /// populate:
    ///
    /// ```ignore
    /// ctx.authorize(PostPolicy::update, &post)?;
    /// ```
    ///
    /// Returning `bool` rather than `Result` keeps policies about *permission*.
    /// A policy that needs to fail some other way — a missing row is a 404, not
    /// a 403 — should say so before the authorization check, not inside it.
    pub fn authorize<T>(&self, policy: impl FnOnce(&Auth, &T) -> bool, subject: &T) -> Result<()> {
        if policy(&self.auth, subject) {
            return Ok(());
        }

        Err(Error::Forbidden)
    }

    /// The same question without the consequence, for deciding what to render.
    pub fn can<T>(&self, policy: impl FnOnce(&Auth, &T) -> bool, subject: &T) -> bool {
        policy(&self.auth, subject)
    }

    pub(crate) fn new(
        request: Request,
        params: Params,
        services: Container,
        config: Config,
    ) -> Self {
        Self {
            request,
            response: Response::default(),
            params,
            extensions: Extensions::new(),
            services,
            auth: Auth::default(),
            config,
            session: Session::detached(),
            inertia: crate::inertia::InertiaRequest::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct CurrentUser(i64);

    #[derive(Debug, PartialEq)]
    struct RequestId(String);

    #[test]
    fn stores_and_reads_values_by_type() {
        let mut extensions = Extensions::new();
        extensions.insert(CurrentUser(7));
        extensions.insert(RequestId("abc".into()));

        assert_eq!(extensions.get::<CurrentUser>(), Some(&CurrentUser(7)));
        assert_eq!(
            extensions.get::<RequestId>(),
            Some(&RequestId("abc".into()))
        );
        assert_eq!(extensions.len(), 2);
    }

    #[test]
    fn inserting_the_same_type_returns_the_previous_value() {
        let mut extensions = Extensions::new();
        assert_eq!(extensions.insert(CurrentUser(1)), None);
        assert_eq!(extensions.insert(CurrentUser(2)), Some(CurrentUser(1)));
        assert_eq!(extensions.get::<CurrentUser>(), Some(&CurrentUser(2)));
    }

    #[test]
    fn missing_types_read_as_none() {
        let extensions = Extensions::new();
        assert_eq!(extensions.get::<CurrentUser>(), None);
        assert!(!extensions.contains::<CurrentUser>());
        assert!(extensions.is_empty());
    }

    #[test]
    fn take_removes_the_value() {
        let mut extensions = Extensions::new();
        extensions.insert(CurrentUser(9));

        assert_eq!(extensions.take::<CurrentUser>(), Some(CurrentUser(9)));
        assert_eq!(extensions.take::<CurrentUser>(), None);
        assert!(extensions.is_empty());
    }
}
