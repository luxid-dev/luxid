//! The service container.
//!
//! Lifetimes follow ASP.NET Core's model, which is the proven answer for a
//! statically typed language: singleton (once per app), scoped (once per
//! request), transient (every resolution).
//!
//! Resolution is by runtime type id — the same trade already taken with
//! pure-context actions. Two things narrow the blast radius:
//!
//! * [`Container::eager_init`] resolves every singleton at boot, so a missing
//!   or cyclic binding fails at startup naming the type, not on first request.
//! * Cycles are detected during resolution and reported as a chain rather than
//!   overflowing the stack.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crate::error::{Error, Result};

type Instance = Arc<dyn Any + Send + Sync>;
type Factory = Arc<dyn Fn(&Container) -> Result<Instance> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifetime {
    /// One instance for the life of the application.
    Singleton,
    /// One instance per request.
    Scoped,
    /// A fresh instance on every resolution.
    Transient,
}

struct Registration {
    type_name: &'static str,
    lifetime: Lifetime,
    factory: Factory,
}

thread_local! {
    /// The chain currently being resolved on this thread, used to turn a
    /// dependency cycle into a readable error instead of a stack overflow.
    static RESOLVING: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

/// Declarative bindings, assembled in `src/app.rs` and frozen into a
/// [`Container`] at boot.
#[derive(Default)]
pub struct Providers {
    registrations: HashMap<TypeId, Registration>,
    order: Vec<TypeId>,
}

impl Providers {
    pub fn new() -> Self {
        Self::default()
    }

    /// One instance for the whole application.
    #[must_use]
    pub fn singleton<T, F>(self, factory: F) -> Self
    where
        T: Any + Send + Sync,
        F: Fn(&Container) -> T + Send + Sync + 'static,
    {
        self.register::<T, _>(Lifetime::Singleton, move |c| Ok(factory(c)))
    }

    /// One instance per request.
    #[must_use]
    pub fn scoped<T, F>(self, factory: F) -> Self
    where
        T: Any + Send + Sync,
        F: Fn(&Container) -> T + Send + Sync + 'static,
    {
        self.register::<T, _>(Lifetime::Scoped, move |c| Ok(factory(c)))
    }

    /// A fresh instance on every resolution.
    #[must_use]
    pub fn transient<T, F>(self, factory: F) -> Self
    where
        T: Any + Send + Sync,
        F: Fn(&Container) -> T + Send + Sync + 'static,
    {
        self.register::<T, _>(Lifetime::Transient, move |c| Ok(factory(c)))
    }

    /// A singleton whose construction can fail — a pool that must connect, a
    /// client that must read credentials. The failure surfaces at boot.
    #[must_use]
    pub fn try_singleton<T, F>(self, factory: F) -> Self
    where
        T: Any + Send + Sync,
        F: Fn(&Container) -> Result<T> + Send + Sync + 'static,
    {
        self.register::<T, _>(Lifetime::Singleton, factory)
    }

    /// Bind a trait object, so apps and tests can swap implementations:
    /// `.bind::<dyn Mailer, _>(|c| Arc::new(Smtp::new(c)))`.
    #[must_use]
    pub fn bind<I, F>(mut self, factory: F) -> Self
    where
        I: ?Sized + Send + Sync + 'static,
        F: Fn(&Container) -> Arc<I> + Send + Sync + 'static,
    {
        let key = TypeId::of::<I>();

        // The instance stored is `Arc<I>` itself, boxed into `Arc<dyn Any>` —
        // an unsized `I` cannot be downcast to directly.
        let registration = Registration {
            type_name: std::any::type_name::<I>(),
            lifetime: Lifetime::Singleton,
            factory: Arc::new(move |c: &Container| {
                let instance: Arc<I> = factory(c);
                Ok(Arc::new(instance) as Instance)
            }),
        };

        if self.registrations.insert(key, registration).is_none() {
            self.order.push(key);
        }
        self
    }

    fn register<T, F>(mut self, lifetime: Lifetime, factory: F) -> Self
    where
        T: Any + Send + Sync,
        F: Fn(&Container) -> Result<T> + Send + Sync + 'static,
    {
        let key = TypeId::of::<T>();

        let registration = Registration {
            type_name: std::any::type_name::<T>(),
            lifetime,
            factory: Arc::new(move |c: &Container| Ok(Arc::new(factory(c)?) as Instance)),
        };

        if self.registrations.insert(key, registration).is_none() {
            self.order.push(key);
        }
        self
    }

    /// Freeze into a container. Call [`Container::eager_init`] to validate.
    pub fn build(self) -> Container {
        Container {
            root: Arc::new(Root {
                registrations: self.registrations,
                order: self.order,
                singletons: RwLock::new(HashMap::new()),
            }),
            scoped: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

struct Root {
    registrations: HashMap<TypeId, Registration>,
    order: Vec<TypeId>,
    singletons: RwLock<HashMap<TypeId, Instance>>,
}

/// A handle onto the application's services. Cheap to clone.
#[derive(Clone)]
pub struct Container {
    root: Arc<Root>,
    scoped: Arc<Mutex<HashMap<TypeId, Instance>>>,
}

impl Default for Container {
    fn default() -> Self {
        Providers::new().build()
    }
}

impl Container {
    /// A per-request view: same singletons, a fresh scoped cache.
    pub fn scope(&self) -> Self {
        Self {
            root: Arc::clone(&self.root),
            scoped: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn contains<T: Any + Send + Sync>(&self) -> bool {
        self.root.registrations.contains_key(&TypeId::of::<T>())
    }

    pub fn is_empty(&self) -> bool {
        self.root.registrations.is_empty()
    }

    pub fn len(&self) -> usize {
        self.root.registrations.len()
    }

    /// Resolve a concrete type.
    pub fn get<T: Any + Send + Sync>(&self) -> Result<Arc<T>> {
        let instance = self.resolve(TypeId::of::<T>(), std::any::type_name::<T>())?;

        instance.downcast::<T>().map_err(|_| {
            Error::internal(format!(
                "`{}` was registered under a different type than it resolves to",
                std::any::type_name::<T>()
            ))
        })
    }

    /// Resolve a trait object registered with [`Providers::bind`].
    pub fn get_dyn<I: ?Sized + Send + Sync + 'static>(&self) -> Result<Arc<I>> {
        let instance = self.resolve(TypeId::of::<I>(), std::any::type_name::<I>())?;

        instance
            .downcast::<Arc<I>>()
            .map(|boxed| Arc::clone(&*boxed))
            .map_err(|_| {
                Error::internal(format!(
                    "`{}` was bound as a concrete type; resolve it with `get` instead of `get_dyn`",
                    std::any::type_name::<I>()
                ))
            })
    }

    /// Resolve every singleton now, in registration order.
    ///
    /// Called by `App::run`, so a missing or cyclic binding is a startup
    /// failure naming the type rather than a 500 at 3am.
    pub fn eager_init(&self) -> Result<()> {
        for key in &self.root.order {
            let registration = &self.root.registrations[key];

            if registration.lifetime == Lifetime::Singleton {
                self.resolve(*key, registration.type_name)?;
            }
        }
        Ok(())
    }

    fn resolve(&self, key: TypeId, requested: &'static str) -> Result<Instance> {
        let Some(registration) = self.root.registrations.get(&key) else {
            return Err(Error::internal(format!(
                "no provider bound for `{requested}`. Register it in `providers()`, \
                 e.g. `.singleton(|_| {requested}::new())`"
            )));
        };

        match registration.lifetime {
            Lifetime::Singleton => {
                if let Some(existing) = self
                    .root
                    .singletons
                    .read()
                    .expect("singleton cache is not poisoned")
                    .get(&key)
                {
                    return Ok(Arc::clone(existing));
                }

                // The factory runs without the lock held: it may resolve other
                // services, and holding the write lock across that would
                // deadlock on any diamond dependency.
                let instance = self.invoke(registration)?;

                let mut singletons = self
                    .root
                    .singletons
                    .write()
                    .expect("singleton cache is not poisoned");

                // A concurrent resolution may have won the race; prefer the
                // instance already published so callers agree on identity.
                Ok(Arc::clone(singletons.entry(key).or_insert(instance)))
            }

            Lifetime::Scoped => {
                if let Some(existing) = self
                    .scoped
                    .lock()
                    .expect("scope cache is not poisoned")
                    .get(&key)
                {
                    return Ok(Arc::clone(existing));
                }

                let instance = self.invoke(registration)?;
                let mut scoped = self.scoped.lock().expect("scope cache is not poisoned");

                Ok(Arc::clone(scoped.entry(key).or_insert(instance)))
            }

            Lifetime::Transient => self.invoke(registration),
        }
    }

    /// Run a factory with cycle detection.
    fn invoke(&self, registration: &Registration) -> Result<Instance> {
        let name = registration.type_name;

        let cycle = RESOLVING.with(|chain| {
            let mut chain = chain.borrow_mut();

            if chain.contains(&name) {
                let mut path = chain.clone();
                path.push(name);
                return Some(path.join(" → "));
            }

            chain.push(name);
            None
        });

        if let Some(path) = cycle {
            // Unwind the marker we did not push, then report the whole chain.
            return Err(Error::internal(format!(
                "dependency cycle in providers: {path}"
            )));
        }

        let outcome = (registration.factory)(self);

        RESOLVING.with(|chain| {
            chain.borrow_mut().pop();
        });

        outcome
    }
}

impl std::fmt::Debug for Container {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Container")
            .field("registered", &self.root.registrations.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct Counter {
        seq: usize,
    }

    impl Counter {
        fn built(counter: &Arc<AtomicUsize>) -> Self {
            Self {
                seq: counter.fetch_add(1, Ordering::SeqCst),
            }
        }
    }

    #[derive(Debug)]
    struct Config {
        url: String,
    }

    #[derive(Debug)]
    struct Pool {
        url: String,
    }

    #[derive(Debug)]
    struct Repo {
        url: String,
    }

    trait Mailer: Send + Sync {
        fn name(&self) -> &'static str;
    }

    #[derive(Debug)]
    struct Smtp;
    impl Mailer for Smtp {
        fn name(&self) -> &'static str {
            "smtp"
        }
    }

    #[test]
    fn a_singleton_is_built_once() {
        let builds = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&builds);

        let container = Providers::new()
            .singleton(move |_| Counter::built(&counter))
            .build();

        let first = container.get::<Counter>().expect("resolves");
        let second = container.get::<Counter>().expect("resolves");

        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.seq, 0);
    }

    #[test]
    fn a_transient_is_built_every_time() {
        let builds = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&builds);

        let container = Providers::new()
            .transient(move |_| Counter::built(&counter))
            .build();

        let first = container.get::<Counter>().expect("resolves");
        let second = container.get::<Counter>().expect("resolves");

        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!((first.seq, second.seq), (0, 1));
    }

    #[test]
    fn a_scoped_value_is_shared_within_a_scope_and_fresh_across_scopes() {
        let builds = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&builds);

        let container = Providers::new()
            .scoped(move |_| Counter::built(&counter))
            .build();

        let request = container.scope();
        let first = request.get::<Counter>().expect("resolves");
        let second = request.get::<Counter>().expect("resolves");
        assert!(Arc::ptr_eq(&first, &second), "same within one request");
        assert_eq!(builds.load(Ordering::SeqCst), 1);

        let other_request = container.scope();
        let third = other_request.get::<Counter>().expect("resolves");
        assert!(!Arc::ptr_eq(&first, &third), "fresh in the next request");
        assert_eq!((first.seq, third.seq), (0, 1));
        assert_eq!(builds.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn singletons_are_shared_across_scopes() {
        let builds = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&builds);

        let container = Providers::new()
            .singleton(move |_| Counter::built(&counter))
            .build();

        let a = container.scope().get::<Counter>().expect("resolves");
        let b = container.scope().get::<Counter>().expect("resolves");

        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(builds.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn factories_can_resolve_their_own_dependencies() {
        let container = Providers::new()
            .singleton(|_| Config {
                url: "postgres://local".into(),
            })
            .singleton(|c| {
                let config = c.get::<Config>().expect("config is registered");
                Pool {
                    url: config.url.clone(),
                }
            })
            .build();

        assert_eq!(
            container.get::<Pool>().expect("resolves").url,
            "postgres://local"
        );
    }

    #[test]
    fn a_diamond_dependency_resolves_without_deadlocking() {
        let container = Providers::new()
            .singleton(|_| Config { url: "u".into() })
            .singleton(|c| Pool {
                url: c.get::<Config>().expect("config").url.clone(),
            })
            .singleton(|c| Repo {
                url: c.get::<Pool>().expect("pool").url.clone(),
            })
            .build();

        container.eager_init().expect("graph resolves");
        assert_eq!(container.get::<Repo>().expect("resolves").url, "u");
    }

    #[test]
    fn an_unregistered_type_names_itself_in_the_error() {
        let container = Providers::new().build();
        let err = container.get::<Config>().unwrap_err();

        let message = format!("{err}");
        assert!(message.contains("no provider bound"), "{message}");
        assert!(message.contains("Config"), "{message}");
        assert!(message.contains("providers()"), "{message}");
    }

    #[test]
    fn eager_init_catches_a_missing_dependency_at_boot() {
        // Pool depends on Config, which nobody registered.
        let container = Providers::new()
            .try_singleton(|c| {
                c.get::<Config>().map(|config| Pool {
                    url: config.url.clone(),
                })
            })
            .build();

        let err = container.eager_init().unwrap_err();
        assert!(format!("{err}").contains("Config"));
    }

    #[test]
    fn eager_init_only_builds_singletons() {
        let builds = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&builds);

        let container = Providers::new()
            .scoped(move |_| Counter::built(&counter))
            .build();

        container.eager_init().expect("nothing eager to build");
        assert_eq!(builds.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_dependency_cycle_is_reported_as_a_chain() {
        let container = Providers::new()
            .try_singleton(|c| {
                c.get::<Repo>().map(|repo| Pool {
                    url: repo.url.clone(),
                })
            })
            .try_singleton(|c| {
                c.get::<Pool>().map(|pool| Repo {
                    url: pool.url.clone(),
                })
            })
            .build();

        let err = container.eager_init().unwrap_err();
        let message = format!("{err}");

        assert!(message.contains("dependency cycle"), "{message}");
        assert!(message.contains("→"), "{message}");
    }

    #[test]
    fn trait_objects_bind_and_resolve() {
        let container = Providers::new()
            .bind::<dyn Mailer, _>(|_| Arc::new(Smtp))
            .build();

        assert_eq!(
            container.get_dyn::<dyn Mailer>().expect("resolves").name(),
            "smtp"
        );
    }

    #[test]
    fn resolving_a_bound_trait_with_get_reports_the_right_call() {
        let container = Providers::new()
            .bind::<dyn Mailer, _>(|_| Arc::new(Smtp))
            .build();

        // `Smtp` itself was never registered — only `dyn Mailer` was.
        assert!(container.get::<Smtp>().is_err());
    }

    #[test]
    fn re_registering_a_type_replaces_it_without_duplicating_boot_order() {
        let container = Providers::new()
            .singleton(|_| Config {
                url: "first".into(),
            })
            .singleton(|_| Config {
                url: "second".into(),
            })
            .build();

        assert_eq!(container.len(), 1);
        assert_eq!(container.get::<Config>().expect("resolves").url, "second");
    }
}
