//! File generation for `luxid new` and `luxid make:model`.
//!
//! Generation is split into planning and writing. A [`Plan`] is a pure value —
//! which files, with what contents, and which lines to insert into existing
//! ones — so the interesting logic is testable without touching a disk.
//!
//! Writing never overwrites. A generator that clobbers hand-written code once
//! is a generator nobody runs again.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use luxid_core::error::{Error, Result};

use crate::naming::Names;

/// A file to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    pub path: PathBuf,
    pub contents: String,
}

/// A line to insert after a marker in an existing file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insertion {
    pub path: PathBuf,
    pub marker: &'static str,
    pub line: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Plan {
    pub files: Vec<NewFile>,
    pub insertions: Vec<Insertion>,
}

impl Plan {
    fn file(&mut self, path: impl Into<PathBuf>, contents: impl Into<String>) {
        self.files.push(NewFile {
            path: path.into(),
            contents: contents.into(),
        });
    }

    fn insert(&mut self, path: impl Into<PathBuf>, marker: &'static str, line: impl Into<String>) {
        self.insertions.push(Insertion {
            path: path.into(),
            marker,
            line: line.into(),
        });
    }

    /// Every path this plan touches, for reporting.
    pub fn touched(&self) -> BTreeSet<&Path> {
        self.files
            .iter()
            .map(|file| file.path.as_path())
            .chain(
                self.insertions
                    .iter()
                    .map(|insertion| insertion.path.as_path()),
            )
            .collect()
    }
}

// ---------------------------------------------------------------- markers

pub const MARK_MODULES: &str = "// <luxid:modules>";
pub const MARK_ROUTES: &str = "// <luxid:routes>";
pub const MARK_MIGRATIONS: &str = "// <luxid:migrations>";
pub const MARK_MIGRATION_MODULES: &str = "// <luxid:migration-modules>";

// ------------------------------------------------------------- luxid new

/// How the generated app should depend on Luxid.
#[derive(Debug, Clone)]
pub enum Dependency {
    /// A published version.
    Version(String),
    /// A local checkout, for working on the framework itself.
    Path(String),
}

impl Dependency {
    fn luxid(&self) -> String {
        match self {
            Self::Version(version) => format!("luxid = \"{version}\""),
            Self::Path(root) => format!("luxid = {{ path = \"{root}/crates/luxid\" }}"),
        }
    }

    fn testing(&self) -> String {
        match self {
            Self::Version(version) => format!("luxid-testing = \"{version}\""),
            Self::Path(root) => {
                format!("luxid-testing = {{ path = \"{root}/crates/luxid-testing\" }}")
            }
        }
    }
}

pub fn new_app(name: &str, dependency: &Dependency) -> Plan {
    let mut plan = Plan::default();
    let crate_name = crate::naming::to_snake(name);

    plan.file(
        "Cargo.toml",
        format!(
            r#"[package]
name = "{crate_name}"
version = "0.1.0"
edition = "2024"

[dependencies]
{}
migration = {{ path = "migration" }}

tokio = {{ version = "1", features = ["macros", "rt-multi-thread"] }}
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
sea-orm = {{ version = "2.0", features = [
    "macros",
    "runtime-tokio-rustls",
    "sqlx-sqlite",
    "sqlx-postgres",
    "with-chrono",
] }}
dotenvy = "0.15"

# Declared directly because their derive macros emit crate-qualified paths:
# re-exporting them through `luxid` is not enough for `#[derive(..)]` to resolve.
schemars = "1.2"

[dev-dependencies]
{}

[profile.dev]
opt-level = 0

# Dependencies are built optimised once; your own crate stays quick to rebuild.
[profile.dev.package."*"]
opt-level = 3
"#,
            dependency.luxid(),
            dependency.testing(),
        ),
    );

    plan.file(
        "migration/Cargo.toml",
        r#"[package]
name = "migration"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
sea-orm-migration = { version = "2.0", default-features = false, features = [
    "runtime-tokio-rustls",
    "sqlx-sqlite",
    "sqlx-postgres",
] }
"#,
    );

    plan.file(
        "migration/src/lib.rs",
        format!(
            r#"pub use sea_orm_migration::prelude::*;

{MARK_MIGRATION_MODULES}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {{
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {{
        vec![
            {MARK_MIGRATIONS}
        ]
    }}
}}
"#
        ),
    );

    plan.file(
        "src/main.rs",
        r#"mod app;
mod config;
mod controllers;
mod entities;
mod factories;
mod middleware;
mod models;
mod policies;
mod routes;
mod seeders;
mod services;
mod validators;

#[tokio::main]
async fn main() -> luxid::Result<()> {
    let _ = dotenvy::dotenv();

    // `cargo run` serves; `cargo luxid routes`, `migrate`, `openapi`
    // and friends are all the same binary.
    luxid::cli::run::<migration::Migrator>(app::build().await?).await
}
"#,
    );

    plan.file(
        "src/app.rs",
        r#"use luxid::prelude::*;

/// Assemble the application: configuration, services, middleware, routes.
pub async fn build() -> luxid::Result<App> {
    // luxid.toml, with the environment layered over it.
    let config = Config::load("luxid.toml")?;

    // Accessing a relation that was not eager-loaded becomes an error, which
    // turns an N+1 into a failing test rather than a production surprise.
    luxid::set_strict_relations(config.get_or("database.strict_relations", cfg!(debug_assertions))?);

    // SQLite by default so a fresh app runs with no infrastructure. Point
    // DATABASE_URL at Postgres when you want one.
    let url = config
        .get_or("database.url", "sqlite://./app.db?mode=rwc".to_owned())?;

    let db = Db::connect(url).await?;

    Ok(App::new()
        .config(config)
        .providers(providers(db))
        .middleware(WithDatabase)
        .routes(crate::routes::register))
}

fn providers(db: Db) -> Providers {
    Providers::new().singleton(move |_| db.clone())
}
"#,
    );

    plan.file(
        "src/routes.rs",
        format!(
            r#"use luxid::prelude::*;

use crate::controllers;

pub fn register(r: &mut Router) {{
    r.group("/api", |r| {{
        r.get("/health", controllers::health_controller::HealthController::show);

        {MARK_ROUTES}
    }});
}}
"#
        ),
    );

    plan.file(
        "src/controllers/mod.rs",
        format!("pub mod health_controller;\n\n{MARK_MODULES}\n"),
    );

    plan.file(
        "src/controllers/health_controller.rs",
        r#"use luxid::prelude::*;
use serde_json::json;

pub struct HealthController;

#[luxid::controller]
impl HealthController {
    #[openapi(summary = "Liveness probe", tag = "system")]
    async fn show(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({ "status": "ok" }))
    }
}
"#,
    );

    // `factories` and `seeders` are here because `make:model` registers into
    // them; a plan that inserts into a file nobody created fails at write time.
    for module in ["models", "entities", "validators", "services", "middleware"] {
        plan.file(format!("src/{module}/mod.rs"), format!("{MARK_MODULES}\n"));
    }

    // These are called from tests, seed commands and controllers you have not
    // written yet — none of which the binary target can see. Without this a
    // freshly generated app greets you with dead-code warnings, which is a good
    // way to teach people to ignore warnings.
    for module in ["policies", "factories", "seeders"] {
        plan.file(
            format!("src/{module}/mod.rs"),
            format!("#![allow(dead_code)]\n\n{MARK_MODULES}\n"),
        );
    }

    plan.file(
        "src/config/mod.rs",
        r#"//! Application configuration. Read from the environment here, and bind the
//! result as a singleton in `app::providers` so actions can resolve it.
"#,
    );

    plan.file(
        ".env.example",
        r#"# Copy to .env and adjust. These override luxid.toml.
DATABASE_URL=sqlite://./app.db?mode=rwc
APP_KEY=change-me-before-deploying
LUXID_ADDR=127.0.0.1:3000
"#,
    );

    plan.file(".gitignore", "/target\n/app.db\n/app.db-*\n.env\n");

    plan.file(
        "luxid.toml",
        format!(
            r#"# Read at boot by `app::build`, and available in any action as
# `ctx.config`. Environment variables override these: `app.name` is also
# `APP_NAME`, `database.url` is also `DATABASE_URL`.

[app]
name = "{crate_name}"
per_page = 20

[database]
# url = "postgres://localhost/{crate_name}"

# Reading a relation that was not eager-loaded becomes an error.
strict_relations = true
"#
        ),
    );

    // Two things live here. The alias is what lets the application's own
    // commands read as `cargo luxid migrate` rather than `cargo run -- migrate`
    // — cargo expands it before dispatch, so nothing in the binary changes and
    // `cargo run -- migrate` keeps working.
    //
    // The linker is shipped commented out on purpose. A faster linker is the
    // biggest single lever on rebuild time, but enabling it by default produces
    // a project that does not compile on any machine without mold — and this
    // file gets committed, so it would break teammates too.
    plan.file(
        ".cargo/config.toml",
        r#"# `cargo luxid <command>` runs this application's own command line.
#
# It expands to `cargo run --`, so `cargo luxid migrate` and
# `cargo run -- migrate` are the same thing. Copy this section into any
# existing project to get the shorter form.
[alias]
luxid = "run --"

# A faster linker is the single biggest lever on rebuild time.
#
# Uncomment after installing mold (https://github.com/rui314/mold), or swap in
# `lld`. Left off by default so this project builds anywhere.
#
# [target.x86_64-unknown-linux-gnu]
# rustflags = ["-C", "link-arg=-fuse-ld=mold", "-C", "debuginfo=1"]
"#,
    );

    plan.file(
        "README.md",
        format!(
            r#"# {crate_name}

```sh
cargo run                 # serve on http://127.0.0.1:3000
cargo luxid routes        # the routing table
cargo luxid migrate       # apply migrations
cargo luxid openapi       # the OpenAPI 3.1 document
```

Generated by `luxid new`.
"#
        ),
    );

    plan
}

// -------------------------------------------------------- luxid make:model

/// Which artefacts `make:model` should produce.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ModelFlags {
    pub migration: bool,
    pub factory: bool,
    pub seeder: bool,
    pub controller: bool,
    pub policy: bool,
    pub requests: bool,
}

impl ModelFlags {
    /// `-a`: everything.
    pub fn all() -> Self {
        Self {
            migration: true,
            factory: true,
            seeder: true,
            controller: true,
            policy: true,
            requests: true,
        }
    }
}

pub fn make_model(names: &Names, flags: ModelFlags, timestamp: &str) -> Plan {
    let mut plan = Plan::default();
    let Names {
        model,
        snake,
        plural,
        ..
    } = names;

    plan.file(
        format!("src/models/{snake}.rs"),
        format!(
            r#"pub use crate::entities::{plural}::Model as {model};

// Relations, scopes and hooks go here. Declare relations in the attribute and
// scopes as `#[scope]` functions:
//
//     #[luxid::model(has_many(posts = Post, fk = "{snake}_id"))]
//     impl {model} {{
//         #[scope]
//         fn active(query: Query<crate::entities::{plural}::Entity>)
//             -> Query<crate::entities::{plural}::Entity>
//         {{
//             query.where_null({model}::deleted_at)
//         }}
//     }}
#[luxid::model()]
impl {model} {{}}
"#
        ),
    );
    plan.insert(
        "src/models/mod.rs",
        MARK_MODULES,
        format!("pub mod {snake};"),
    );

    // The entity is regenerated from the schema by `luxid db:sync`; this stub
    // exists so the app compiles before the first migration has run.
    plan.file(
        format!("src/entities/{plural}.rs"),
        format!(
            r#"//! Generated from the database schema. Re-run `luxid db:sync` after a
//! migration rather than editing this by hand.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, serde::Serialize, luxid::Model)]
#[sea_orm(table_name = "{plural}")]
pub struct Model {{
    // <luxid:fields>  refreshed by `cargo luxid db:sync`
    #[sea_orm(primary_key)]
    pub id: i64,
    // </luxid:fields>
    #[sea_orm(ignore)]
    #[serde(flatten)]
    pub relations: luxid::Relations,
}}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {{}}

impl ActiveModelBehavior for ActiveModel {{}}
"#
        ),
    );
    plan.insert(
        "src/entities/mod.rs",
        MARK_MODULES,
        format!("pub mod {plural};"),
    );

    if flags.migration {
        let migration = format!("m{timestamp}_create_{plural}");

        plan.file(
            format!("migration/src/{migration}.rs"),
            format!(
                r#"use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveIden)]
enum {} {{
    Table,
    Id,
}}

pub struct Migration;

impl MigrationName for Migration {{
    fn name(&self) -> &str {{
        "{migration}"
    }}
}}

#[async_trait::async_trait]
impl MigrationTrait for Migration {{
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {{
        manager
            .create_table(
                Table::create()
                    .table({0}::Table)
                    .if_not_exists()
                    .col(pk_auto({0}::Id))
                    // Add your columns here.
                    .to_owned(),
            )
            .await
    }}

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {{
        manager.drop_table(Table::drop().table({0}::Table).to_owned()).await
    }}
}}
"#,
                crate::naming::to_pascal(plural)
            ),
        );

        plan.insert(
            "migration/src/lib.rs",
            MARK_MIGRATION_MODULES,
            format!("mod {migration};"),
        );
        plan.insert(
            "migration/src/lib.rs",
            MARK_MIGRATIONS,
            format!("Box::new({migration}::Migration),"),
        );
    }

    if flags.requests {
        let store = &names.store_request;
        let update = &names.update_request;

        plan.file(
            format!("src/validators/{snake}.rs"),
            format!(
                r#"use luxid::prelude::*;
use serde::Deserialize;

// `JsonSchema` lets `#[openapi(body = ..)]` describe this request.
#[derive(Debug, Deserialize, Validate, luxid::JsonSchema)]
pub struct {store} {{
    // #[validate(length(min = 1, max = 255))]
    // pub name: String,
}}

#[derive(Debug, Deserialize, Validate, luxid::JsonSchema)]
pub struct {update} {{
    // #[validate(length(min = 1, max = 255))]
    // pub name: Option<String>,
}}
"#
            ),
        );
        plan.insert(
            "src/validators/mod.rs",
            MARK_MODULES,
            format!("pub mod {snake};"),
        );
    }

    if flags.policy {
        let policy = &names.policy;

        plan.file(
            format!("src/policies/{snake}_policy.rs"),
            format!(
                r#"use luxid::prelude::*;

use crate::models::{snake}::{model};

pub struct {policy};

/// A policy is an ordinary function of `(&Auth, &T) -> bool`. Enforce one with
/// `ctx.authorize({policy}::update, &{snake})?`, or ask without consequence
/// with `ctx.can({policy}::update, &{snake})`.
impl {policy} {{
    pub fn view(_auth: &Auth, _{snake}: &{model}) -> bool {{
        true
    }}

    pub fn update(auth: &Auth, {snake}: &{model}) -> bool {{
        // Replace with your rule, e.g. comparing the owner to the identity.
        let _ = {snake};
        auth.check()
    }}
}}
"#
            ),
        );
        plan.insert(
            "src/policies/mod.rs",
            MARK_MODULES,
            format!("pub mod {snake}_policy;"),
        );
    }

    if flags.factory {
        let factory = &names.factory;

        plan.file(
            format!("src/factories/{snake}_factory.rs"),
            format!(
                r#"use luxid::prelude::*;

// Used by the field list below, which `cargo luxid db:sync` fills in from the
// live schema. Allowed rather than left to warn: a table whose columns are all
// nullable syncs to an empty list, so the import can be legitimately unused.
#[allow(unused_imports)]
use sea_orm::ActiveValue::Set;

use crate::entities::{plural};

/// The typical {model}. Tests override only what they care about:
///
/// ```ignore
/// {factory}::new().create_one().await?;
/// {factory}::new().count(3).create().await?;
/// {factory}::new().state(|row| row.name = Set("Ada".into())).create_one().await?;
/// ```
pub struct {factory};

impl Factory for {factory} {{
    type Active = {plural}::ActiveModel;

    fn definition() -> Self::Active {{
        {plural}::ActiveModel {{
            // <luxid:fields>  refreshed by `cargo luxid db:sync`
            // </luxid:fields>
            ..Default::default()
        }}
    }}
}}
"#
            ),
        );
        plan.insert(
            "src/factories/mod.rs",
            MARK_MODULES,
            format!("pub mod {snake}_factory;"),
        );
    }

    if flags.seeder {
        let seeder = &names.seeder;
        let factory = &names.factory;

        plan.file(
            format!("src/seeders/{snake}_seeder.rs"),
            format!(
                r#"use luxid::prelude::*;

use crate::factories::{snake}_factory::{factory};

pub struct {seeder};

impl {seeder} {{
    pub async fn run() -> luxid::Result<()> {{
        {factory}::new().count(10).create().await?;
        Ok(())
    }}
}}
"#
            ),
        );
        plan.insert(
            "src/seeders/mod.rs",
            MARK_MODULES,
            format!("pub mod {snake}_seeder;"),
        );
    }

    if flags.controller {
        let controller = &names.controller;
        let controller_file = &names.controller_file;

        plan.file(
            format!("src/controllers/{controller_file}.rs"),
            format!(
                r#"use luxid::prelude::*;

use crate::models::{snake}::{model};

pub struct {controller};

#[luxid::controller]
impl {controller} {{
    #[openapi(tag = "{plural}")]
    async fn index(ctx: HttpContext) -> Result<Response> {{
        let page = ctx.request.input::<u64>("page")?.unwrap_or(1);

        ctx.response.ok({model}::query().paginate(page, 20).await?)
    }}

    #[openapi(tag = "{plural}", errors = [404])]
    async fn show(ctx: HttpContext) -> Result<Response> {{
        let id: i64 = ctx.params.get("id")?;

        ctx.response.ok({model}::find_or_fail(id).await?)
    }}

    #[openapi(tag = "{plural}", errors = [422])]
    async fn store(ctx: HttpContext) -> Result<Response> {{
        let _input = ctx.request.validate::<crate::validators::{snake}::{}>().await?;

        ctx.response.status(501).json("not implemented")
    }}

    #[openapi(tag = "{plural}", errors = [404, 422])]
    async fn update(ctx: HttpContext) -> Result<Response> {{
        let id: i64 = ctx.params.get("id")?;
        let existing = {model}::find_or_fail(id).await?;

        ctx.authorize(crate::policies::{snake}_policy::{}::update, &existing)?;

        let _input = ctx.request.validate::<crate::validators::{snake}::{}>().await?;

        ctx.response.status(501).json("not implemented")
    }}

    #[openapi(tag = "{plural}", no_content, errors = [404])]
    async fn destroy(ctx: HttpContext) -> Result<Response> {{
        let id: i64 = ctx.params.get("id")?;
        {model}::find_or_fail(id).await?;

        ctx.response.no_content()
    }}
}}
"#,
                names.store_request, names.policy, names.update_request
            ),
        );
        plan.insert(
            "src/controllers/mod.rs",
            MARK_MODULES,
            format!("pub mod {controller_file};"),
        );

        // One line, registering only the actions the controller defines.
        plan.insert(
            "src/routes.rs",
            MARK_ROUTES,
            format!("r.resource(\"/{plural}\", controllers::{controller_file}::{controller});"),
        );
    }

    plan
}

// -------------------------------------------------------------- writing

/// Write a plan to disk.
///
/// Refuses if any file already exists, and writes nothing in that case: a
/// half-applied generator is worse than one that declined.
pub fn write(plan: &Plan, root: &Path) -> Result<Vec<PathBuf>> {
    let mut clashes = Vec::new();
    for file in &plan.files {
        if root.join(&file.path).exists() {
            clashes.push(file.path.display().to_string());
        }
    }

    if !clashes.is_empty() {
        return Err(Error::internal(format!(
            "these files already exist, so nothing was written: {}",
            clashes.join(", ")
        )));
    }

    let mut written = Vec::new();

    for file in &plan.files {
        let target = root.join(&file.path);

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| Error::internal(format!("could not create {parent:?}: {err}")))?;
        }

        std::fs::write(&target, &file.contents)
            .map_err(|err| Error::internal(format!("could not write {target:?}: {err}")))?;

        written.push(file.path.clone());
    }

    for insertion in &plan.insertions {
        let target = root.join(&insertion.path);

        let existing = std::fs::read_to_string(&target)
            .map_err(|err| Error::internal(format!("could not read {target:?}: {err}")))?;

        let updated = apply(&existing, insertion.marker, &insertion.line).ok_or_else(|| {
            Error::internal(format!(
                "{:?} has no `{}` marker, so there is nowhere to register this. \
                 Add the marker back, or wire it up by hand.",
                insertion.path, insertion.marker
            ))
        })?;

        std::fs::write(&target, updated)
            .map_err(|err| Error::internal(format!("could not write {target:?}: {err}")))?;

        written.push(insertion.path.clone());
    }

    written.sort();
    written.dedup();

    Ok(written)
}

/// Insert `line` immediately above `marker`, matching the marker's indentation.
///
/// Above rather than below so repeated generation keeps chronological order:
/// the marker stays at the bottom of its list.
pub fn apply(source: &str, marker: &str, line: &str) -> Option<String> {
    let position = source.find(marker)?;
    let line_start = source[..position]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let indent = &source[line_start..position];

    let inserted: String = line
        .lines()
        .map(|part| format!("{indent}{part}\n"))
        .collect::<Vec<_>>()
        .join("");

    let mut out = String::with_capacity(source.len() + inserted.len());
    out.push_str(&source[..line_start]);
    out.push_str(&inserted);
    out.push_str(&source[line_start..]);

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_above_the_marker_with_its_indentation() {
        let source = "fn main() {\n    let x = 1;\n    // <luxid:routes>\n}\n";
        let updated = apply(source, MARK_ROUTES, "call_me();").expect("marker present");

        assert_eq!(
            updated,
            "fn main() {\n    let x = 1;\n    call_me();\n    // <luxid:routes>\n}\n"
        );
    }

    #[test]
    fn repeated_insertion_keeps_order() {
        let source = "// <luxid:modules>\n";
        let once = apply(source, MARK_MODULES, "pub mod a;").expect("marker");
        let twice = apply(&once, MARK_MODULES, "pub mod b;").expect("marker");

        assert_eq!(twice, "pub mod a;\npub mod b;\n// <luxid:modules>\n");
    }

    #[test]
    fn multi_line_insertions_are_indented_line_by_line() {
        let source = "        // <luxid:routes>\n";
        let updated = apply(source, MARK_ROUTES, "one();\ntwo();").expect("marker");

        assert_eq!(
            updated,
            "        one();\n        two();\n        // <luxid:routes>\n"
        );
    }

    #[test]
    fn a_missing_marker_is_reported_rather_than_guessed() {
        assert!(apply("fn main() {}\n", MARK_ROUTES, "x();").is_none());
    }

    #[test]
    fn a_new_app_carries_every_marker_make_model_needs() {
        let plan = new_app("my-app", &Dependency::Version("0.2".into()));

        let find = |path: &str| {
            plan.files
                .iter()
                .find(|file| file.path == Path::new(path))
                .unwrap_or_else(|| panic!("{path} missing from the plan"))
        };

        assert!(find("src/routes.rs").contents.contains(MARK_ROUTES));
        assert!(find("src/models/mod.rs").contents.contains(MARK_MODULES));
        assert!(find("src/entities/mod.rs").contents.contains(MARK_MODULES));
        assert!(
            find("src/controllers/mod.rs")
                .contents
                .contains(MARK_MODULES)
        );
        assert!(find("src/factories/mod.rs").contents.contains(MARK_MODULES));
        assert!(find("src/seeders/mod.rs").contents.contains(MARK_MODULES));
        assert!(
            find("migration/src/lib.rs")
                .contents
                .contains(MARK_MIGRATIONS)
        );
        assert!(
            find("migration/src/lib.rs")
                .contents
                .contains(MARK_MIGRATION_MODULES)
        );
    }

    #[test]
    fn a_hyphenated_app_name_becomes_a_valid_crate_name() {
        let plan = new_app("my-app", &Dependency::Version("0.2".into()));
        let manifest = &plan.files[0].contents;

        assert!(manifest.contains(r#"name = "my_app""#), "{manifest}");
    }

    #[test]
    fn a_bare_model_generates_only_the_model_and_entity() {
        let plan = make_model(
            &Names::new("User"),
            ModelFlags::default(),
            "20260101_000001",
        );

        let paths: Vec<_> = plan
            .files
            .iter()
            .map(|file| file.path.display().to_string())
            .collect();

        assert_eq!(paths, vec!["src/models/user.rs", "src/entities/users.rs"]);
    }

    #[test]
    fn the_all_flag_generates_every_artefact() {
        let plan = make_model(&Names::new("User"), ModelFlags::all(), "20260101_000001");

        let paths: Vec<_> = plan
            .files
            .iter()
            .map(|file| file.path.display().to_string())
            .collect();

        for expected in [
            "src/models/user.rs",
            "src/entities/users.rs",
            "migration/src/m20260101_000001_create_users.rs",
            "src/validators/user.rs",
            "src/policies/user_policy.rs",
            "src/factories/user_factory.rs",
            "src/seeders/user_seeder.rs",
            "src/controllers/users_controller.rs",
        ] {
            assert!(
                paths.contains(&expected.to_owned()),
                "{expected} missing from {paths:?}"
            );
        }
    }

    #[test]
    fn a_controller_registers_one_resource_line() {
        let plan = make_model(
            &Names::new("User"),
            ModelFlags {
                controller: true,
                ..ModelFlags::default()
            },
            "20260101_000001",
        );

        let routes = plan
            .insertions
            .iter()
            .find(|insertion| insertion.marker == MARK_ROUTES)
            .expect("routes registered");

        // One line, not five: `resource` registers only the actions the
        // controller actually defines.
        assert_eq!(routes.line.lines().count(), 1);
        assert!(
            routes.line.starts_with(r#"r.resource("/users", "#),
            "{}",
            routes.line
        );
        assert!(routes.line.contains("UsersController"));
    }

    #[test]
    fn migrations_register_both_the_module_and_the_boxed_value() {
        let plan = make_model(
            &Names::new("User"),
            ModelFlags {
                migration: true,
                ..ModelFlags::default()
            },
            "20260101_000001",
        );

        let markers: Vec<_> = plan
            .insertions
            .iter()
            .map(|insertion| insertion.marker)
            .collect();

        assert!(markers.contains(&MARK_MIGRATION_MODULES));
        assert!(markers.contains(&MARK_MIGRATIONS));
    }
}

pub const MARK_FIELDS_OPEN: &str = "// <luxid:fields>";
pub const MARK_FIELDS_CLOSE: &str = "// </luxid:fields>";

/// Replace the body between the field markers, preserving everything else.
///
/// Only the region between the markers is touched, so hand-written rules and
/// overrides outside it survive a resync. This is the piece Laravel's stub
/// generators never had.
pub fn refresh_fields(source: &str, body: &str) -> Option<String> {
    let open = source.find(MARK_FIELDS_OPEN)?;
    let close = source[open..].find(MARK_FIELDS_CLOSE)? + open;

    let line_start = source[..open]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let indent = &source[line_start..open];

    let open_line_end = source[open..].find('\n')? + open + 1;

    let rendered: String = body
        .lines()
        .map(|line| format!("{indent}{line}\n"))
        .collect::<Vec<_>>()
        .join("");

    let mut out = String::with_capacity(source.len() + rendered.len());
    out.push_str(&source[..open_line_end]);
    out.push_str(&rendered);
    out.push_str(&source[line_start..]);

    // `source[line_start..]` re-emits the indent before the closing marker.
    let tail_start = close - indent.len();
    out.truncate(open_line_end + rendered.len());
    out.push_str(&source[tail_start..]);

    Some(out)
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

    const SOURCE: &str = "fn definition() {\n    ActiveModel {\n        // <luxid:fields>\n        old: Set(1),\n        // </luxid:fields>\n        ..Default::default()\n    }\n}\n";

    #[test]
    fn replaces_only_the_marked_region() {
        let updated = refresh_fields(SOURCE, "name: Set(\"name\".to_owned()),").expect("markers");

        assert!(updated.contains("name: Set(\"name\".to_owned()),"));
        assert!(
            !updated.contains("old: Set(1)"),
            "the old body was replaced"
        );

        // Everything outside the markers survives.
        assert!(updated.contains("..Default::default()"));
        assert!(updated.starts_with("fn definition() {"));
        assert!(updated.trim_end().ends_with('}'));
    }

    #[test]
    fn is_idempotent() {
        let once = refresh_fields(SOURCE, "a: Set(1),").expect("markers");
        let twice = refresh_fields(&once, "a: Set(1),").expect("markers");

        assert_eq!(once, twice, "resyncing an unchanged schema changes nothing");
    }

    #[test]
    fn keeps_the_marker_indentation() {
        let updated = refresh_fields(SOURCE, "a: Set(1),").expect("markers");
        assert!(updated.contains("\n        a: Set(1),\n"), "{updated}");
    }

    #[test]
    fn an_empty_body_clears_the_region() {
        let updated = refresh_fields(SOURCE, "").expect("markers");
        assert!(!updated.contains("old: Set(1)"));
    }

    #[test]
    fn a_file_without_markers_is_left_alone() {
        assert!(refresh_fields("fn nothing() {}", "a: Set(1),").is_none());
    }
}

/// Attributes a user wrote above each field inside the markers.
///
/// `db:sync` regenerates the field list, which would otherwise discard things
/// like `#[serde(skip_serializing)]` on a password hash — a silent change from
/// "never sent" to "sent to every client". Attributes are carried across
/// instead.
///
/// `#[sea_orm(primary_key)]` is excluded because the generator emits it itself.
pub fn field_attributes(source: &str) -> BTreeMap<String, Vec<String>> {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();

    let Some(open) = source.find(MARK_FIELDS_OPEN) else {
        return found;
    };
    let Some(close) = source[open..]
        .find(MARK_FIELDS_CLOSE)
        .map(|index| index + open)
    else {
        return found;
    };

    let mut pending: Vec<String> = Vec::new();

    for line in source[open..close].lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("#[") {
            if trimmed != "#[sea_orm(primary_key)]" {
                pending.push(trimmed.to_owned());
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("pub ")
            && let Some((name, _)) = rest.split_once(':')
            && !pending.is_empty()
        {
            found.insert(name.trim().to_owned(), std::mem::take(&mut pending));
            continue;
        }

        pending.clear();
    }

    found
}

#[cfg(test)]
mod attribute_tests {
    use super::*;

    const SOURCE: &str = "pub struct Model {\n    // <luxid:fields>\n    #[sea_orm(primary_key)]\n    pub id: i64,\n    #[serde(skip_serializing)]\n    pub password: String,\n    pub email: String,\n    // </luxid:fields>\n}\n";

    #[test]
    fn carries_user_written_attributes_across_a_resync() {
        let found = field_attributes(SOURCE);

        assert_eq!(
            found.get("password"),
            Some(&vec!["#[serde(skip_serializing)]".to_owned()])
        );
        assert_eq!(found.get("email"), None, "no attributes, nothing to carry");
    }

    #[test]
    fn the_generated_primary_key_attribute_is_not_carried() {
        // The generator emits it, so carrying it too would duplicate it.
        assert_eq!(field_attributes(SOURCE).get("id"), None);
    }

    #[test]
    fn a_file_without_markers_yields_nothing() {
        assert!(field_attributes("pub struct Model {}").is_empty());
    }
}
