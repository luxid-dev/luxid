//! The database handle and the request-scoped connection.
//!
//! Lucid's ergonomics — `User::find(id)`, `User::query().where_eq(..)` — depend
//! on an *ambient* connection: those call sites have nowhere to pass a `&db`.
//!
//! Luxid makes it ambient with a tokio task-local rather than a global. That
//! buys the Eloquent call shape without process-wide state, and it is the same
//! mechanism that makes transaction-per-test rollback work: a test scopes a
//! transaction handle, and every query inside that scope joins it
//! automatically, with no cooperation from the code under test.
//!
//! The failure mode is a detached `tokio::spawn`, which does not inherit
//! task-locals. That reports an error naming the fix rather than silently
//! reaching for a different connection.

use std::sync::Arc;

use luxid_core::error::{Error, Result};
use sea_orm::{DatabaseConnection, DatabaseTransaction, TransactionTrait};

/// A connection or an open transaction. Queries do not care which.
#[derive(Clone)]
pub enum Handle {
    Pool(Arc<DatabaseConnection>),
    Transaction(Arc<DatabaseTransaction>),
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pool(_) => f.write_str("Handle::Pool"),
            Self::Transaction(_) => f.write_str("Handle::Transaction"),
        }
    }
}

tokio::task_local! {
    /// The connection in scope for the current task.
    static CURRENT: Handle;
}

/// The application's database, registered as a singleton in `providers()`.
#[derive(Clone, Debug)]
pub struct Db {
    pool: Arc<DatabaseConnection>,
}

impl Db {
    pub async fn connect(url: impl Into<String>) -> Result<Self> {
        let pool = sea_orm::Database::connect(url.into())
            .await
            .map_err(|err| Error::internal(format!("could not connect to the database: {err}")))?;

        Ok(Self {
            pool: Arc::new(pool),
        })
    }

    /// An isolated in-memory SQLite database.
    ///
    /// The pool is capped at one connection deliberately: SQLite gives every
    /// *connection* its own `:memory:` database, so a larger pool would hand
    /// out connections that cannot see each other's tables.
    pub async fn in_memory() -> Result<Self> {
        let mut options = sea_orm::ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);

        let pool = sea_orm::Database::connect(options).await.map_err(|err| {
            Error::internal(format!("could not open an in-memory database: {err}"))
        })?;

        Ok(Self {
            pool: Arc::new(pool),
        })
    }

    pub fn from_connection(connection: DatabaseConnection) -> Self {
        Self {
            pool: Arc::new(connection),
        }
    }

    pub fn connection(&self) -> &DatabaseConnection {
        &self.pool
    }

    /// Run `work` with this database ambient, so `User::find(..)` inside it
    /// resolves. The request adapter wraps every action in one of these.
    pub async fn scope<T>(&self, work: impl Future<Output = T>) -> T {
        CURRENT
            .scope(Handle::Pool(Arc::clone(&self.pool)), work)
            .await
    }

    /// Run `work` inside a transaction that is **always rolled back**.
    ///
    /// This is the harness behind `#[luxid::test]`: tests share one database,
    /// run in parallel, and leave nothing behind — no truncation, no fixtures,
    /// no ordering constraints.
    pub async fn rollback_scope<T>(&self, work: impl AsyncFnOnce() -> T) -> Result<T> {
        let transaction = self
            .pool
            .begin()
            .await
            .map_err(|err| Error::internal(format!("could not begin a transaction: {err}")))?;

        let handle = Handle::Transaction(Arc::new(transaction));
        let outcome = CURRENT.scope(handle.clone(), work()).await;

        // The Arc held by the scope is dropped by now, so unwrapping is
        // infallible in practice; if user code stashed a clone, rolling back
        // is skipped rather than panicking.
        if let Handle::Transaction(transaction) = handle
            && let Some(transaction) = Arc::into_inner(transaction)
        {
            transaction
                .rollback()
                .await
                .map_err(|err| Error::internal(format!("could not roll back: {err}")))?;
        }

        Ok(outcome)
    }

    /// Run `work` in a transaction, committing on `Ok` and rolling back on
    /// `Err`.
    pub async fn transaction<T>(&self, work: impl AsyncFnOnce() -> Result<T>) -> Result<T> {
        let transaction = self
            .pool
            .begin()
            .await
            .map_err(|err| Error::internal(format!("could not begin a transaction: {err}")))?;

        let handle = Handle::Transaction(Arc::new(transaction));
        let outcome = CURRENT.scope(handle.clone(), work()).await;

        let Handle::Transaction(transaction) = handle else {
            unreachable!("the handle was just constructed as a transaction");
        };
        let Some(transaction) = Arc::into_inner(transaction) else {
            return Err(Error::internal(
                "a database handle outlived its transaction; do not store `Handle` across scopes",
            ));
        };

        match outcome {
            Ok(value) => {
                transaction.commit().await.map_err(|err| {
                    Error::internal(format!("could not commit the transaction: {err}"))
                })?;
                Ok(value)
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }
}

/// The connection in scope, or a diagnosable error.
pub fn current() -> Result<Handle> {
    CURRENT.try_with(Handle::clone).map_err(|_| {
        Error::internal(
            "no database connection is in scope. Queries must run inside a request, \
             a `Db::scope`, or a `#[luxid::test]`. A detached `tokio::spawn` does not \
             inherit the scope — pass the handle in, or spawn inside `Db::scope`.",
        )
    })
}

/// Run a query against whichever handle is in scope.
///
/// A macro rather than a function because `Pool` and `Transaction` are
/// different `ConnectionTrait` types, and unifying them behind `dyn` would
/// forfeit SeaORM's generic execution path.
#[macro_export]
macro_rules! with_connection {
    ($handle:expr, |$conn:ident| $body:expr) => {
        match $handle {
            $crate::db::Handle::Pool(pool) => {
                let $conn = pool.as_ref();
                $body
            }
            $crate::db::Handle::Transaction(transaction) => {
                let $conn = transaction.as_ref();
                $body
            }
        }
    };
}

/// Middleware that puts the application's database in scope for the request.
///
/// `luxid-core` cannot depend on `luxid-orm` — the dependency runs the other
/// way — so the connection scope arrives as middleware rather than as a field
/// the adapter sets. Register it once globally:
///
/// ```ignore
/// App::new()
///     .providers(Providers::new().singleton(move |_| db.clone()))
///     .middleware(WithDatabase)
/// ```
pub struct WithDatabase;

impl luxid_core::middleware::Middleware for WithDatabase {
    fn handle<'a>(
        &'a self,
        ctx: luxid_core::HttpContext,
        next: luxid_core::middleware::Next,
    ) -> luxid_core::middleware::BoxFuture<'a, Result<luxid_core::Response>> {
        Box::pin(async move {
            // If a handle is already in scope — a test running inside
            // `rollback_scope`, say — join it rather than reaching for the
            // pool. Overriding here would strand the request outside the
            // caller's transaction, and with a single-connection pool it would
            // deadlock against the connection that transaction holds.
            if current().is_ok() {
                return next.run(ctx).await;
            }

            let db = ctx.services.get::<Db>()?;
            db.scope(next.run(ctx)).await
        })
    }
}

/// As [`WithDatabase`], but every request runs in a transaction that is rolled
/// back afterwards. Intended for tests, never for production.
pub struct WithRollbackDatabase;

impl luxid_core::middleware::Middleware for WithRollbackDatabase {
    fn handle<'a>(
        &'a self,
        ctx: luxid_core::HttpContext,
        next: luxid_core::middleware::Next,
    ) -> luxid_core::middleware::BoxFuture<'a, Result<luxid_core::Response>> {
        Box::pin(async move {
            // Already inside a transaction: nesting another would be a
            // savepoint at best, and would fight an outer rollback.
            if current().is_ok() {
                return next.run(ctx).await;
            }

            let db = ctx.services.get::<Db>()?;
            db.rollback_scope(async || next.run(ctx).await).await?
        })
    }
}
