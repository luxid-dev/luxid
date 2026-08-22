//! Luxid — a convention-over-configuration web framework for Rust.
//!
//! ```ignore
//! use luxid::prelude::*;
//!
//! pub struct UsersController;
//!
//! #[luxid::controller]
//! impl UsersController {
//!     async fn index(ctx: HttpContext) -> Result<Response> {
//!         let page = ctx.request.input::<u32>("page")?.unwrap_or(1);
//!         ctx.response.ok(serde_json::json!({ "page": page }))
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() -> luxid::Result<()> {
//!     App::new()
//!         .routes(|r| {
//!             r.group("/api/v1", |r| {
//!                 r.get("/users", UsersController::index);
//!             });
//!         })
//!         .run()
//!         .await
//! }
//! ```

pub use luxid_core::{
    Action, App, Auth, Body, BoxFuture, Config, Container, Cookie, Error, Extensions, Hash,
    HttpContext, Identity, Jwt, Lifetime, MemoryStore, Method, Middleware, Next, Params, Providers,
    Request, Response, Result, Route, Router, SameSite, Session, SessionData, SessionStore,
    Validate, ValidationErrors, validate,
};
pub use luxid_core::{
    adapter, app, auth, container, context, error, http, middleware as mw, router, session,
};
#[cfg(feature = "orm")]
pub use luxid_macros::model;
pub use luxid_macros::{Validate, controller, middleware, test};

/// Derive Lucid operations and typed columns for a SeaORM entity model.
///
/// Typed columns are the point: a column's `Value` is its actual Rust type, so
/// a mismatched comparison is a compile error rather than a runtime surprise.
///
/// This compiles — `published` is an `i32`:
///
/// ```no_run
/// # use luxid::prelude::*;
/// mod posts {
///     use sea_orm::entity::prelude::*;
///
///     #[derive(Clone, Debug, PartialEq, DeriveEntityModel, luxid::Model)]
///     #[sea_orm(table_name = "posts")]
///     pub struct Model {
///         #[sea_orm(primary_key)]
///         pub id: i64,
///         pub published: i32,
///     }
///
///     #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
///     pub enum Relation {}
///     impl ActiveModelBehavior for ActiveModel {}
/// }
///
/// let _ = posts::Model::query().where_eq(posts::Model::published, 1);
/// ```
///
/// This does not — a `&str` is not an `i32`:
///
/// ```compile_fail
/// # use luxid::prelude::*;
/// mod posts {
///     use sea_orm::entity::prelude::*;
///
///     #[derive(Clone, Debug, PartialEq, DeriveEntityModel, luxid::Model)]
///     #[sea_orm(table_name = "posts")]
///     pub struct Model {
///         #[sea_orm(primary_key)]
///         pub id: i64,
///         pub published: i32,
///     }
///
///     #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
///     pub enum Relation {}
///     impl ActiveModelBehavior for ActiveModel {}
/// }
///
/// let _ = posts::Model::query().where_eq(posts::Model::published, "not a number");
/// ```
///
/// The entity's own `Column` enum stays available as an untyped escape hatch,
/// accepting anything SeaORM would:
///
/// ```no_run
/// # use luxid::prelude::*;
/// # mod posts {
/// #     use sea_orm::entity::prelude::*;
/// #     #[derive(Clone, Debug, PartialEq, DeriveEntityModel, luxid::Model)]
/// #     #[sea_orm(table_name = "posts")]
/// #     pub struct Model {
/// #         #[sea_orm(primary_key)]
/// #         pub id: i64,
/// #         pub published: i32,
/// #     }
/// #     #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
/// #     pub enum Relation {}
/// #     impl ActiveModelBehavior for ActiveModel {}
/// # }
/// let _ = posts::Model::query().where_eq(posts::Column::Published, 1);
/// ```
#[cfg(feature = "orm")]
pub use luxid_macros::Model;
#[cfg(feature = "orm")]
pub use luxid_orm::model::{ColumnRef, delete_by_id, insert, insert_without_hooks, update};
/// The Lucid data layer.
#[cfg(feature = "orm")]
pub use luxid_orm::{
    Db, Factory, FactoryBuilder, Hooks, Lucid, Paginated, Query, Relatable, Relations,
    WithDatabase, WithRollbackDatabase, sea_orm, set_strict_relations, strict_relations,
};

/// The application command line.
#[cfg(feature = "cli")]
pub mod cli {
    pub use luxid_cli::{render_routes, run};
}

#[cfg(feature = "orm")]
pub use luxid_orm::{MigrationState, migration};

/// Re-exported so applications can derive schemas without declaring schemars.
pub use schemars::{self, JsonSchema};

#[doc(hidden)]
pub mod __private {
    pub use luxid_core::__private::*;

    /// Render a type's JSON Schema for the OpenAPI document.
    ///
    /// Lives here rather than in `luxid-core` so the core crate needs no
    /// JSON-schema dependency; the call is generated in the application's
    /// crate, where the type is.
    pub fn schema_of<T: schemars::JsonSchema>() -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(T)).unwrap_or_else(|_| serde_json::json!({}))
    }

    #[cfg(feature = "orm")]
    pub use luxid_orm::__private::*;
}

pub mod prelude {
    pub use crate::{
        App, Auth, Config, Container, Cookie, Error, Hash, HttpContext, Identity, Jwt, MemoryStore,
        Middleware, Next, Params, Providers, Request, Response, Result, Router, Session,
        SessionStore, Validate, ValidationErrors, controller, middleware,
    };

    #[cfg(feature = "orm")]
    pub use crate::{Db, Factory, Lucid, Paginated, WithDatabase, WithRollbackDatabase};
}
