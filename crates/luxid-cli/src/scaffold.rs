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

/// What kind of application `luxid new` produces.
///
/// This is a scaffolding decision, not a framework one: an app can move from
/// one to the other by hand afterwards, and the Inertia middleware is available
/// to any app that wants it. Inertia is deliberately **not** the default —
/// most Luxid apps are APIs, and a Node toolchain is a real cost to impose on
/// someone who did not ask for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stack {
    /// JSON endpoints only. No frontend, no Node.
    #[default]
    Api,
    /// Server-driven pages rendered by a JavaScript client over the Inertia
    /// protocol.
    Inertia(Client),
}

/// The client framework an Inertia app is built with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Client {
    #[default]
    React,
    Vue,
    Svelte,
}

impl Client {
    pub fn label(self) -> &'static str {
        match self {
            Self::React => "react",
            Self::Vue => "vue",
            Self::Svelte => "svelte",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "react" => Some(Self::React),
            "vue" => Some(Self::Vue),
            "svelte" => Some(Self::Svelte),
            _ => None,
        }
    }

    /// Extension of the client entry point. This doubles as the manifest key
    /// the server looks up, so it must match `vite.config.js`'s input.
    fn entry_ext(self) -> &'static str {
        match self {
            // JSX must be spelled out; the others are plain modules.
            Self::React => "jsx",
            Self::Vue | Self::Svelte => "js",
        }
    }

    fn entry(self) -> String {
        format!("resources/js/app.{}", self.entry_ext())
    }

    /// Extension of a page component.
    fn page_ext(self) -> &'static str {
        match self {
            Self::React => "jsx",
            Self::Vue => "vue",
            Self::Svelte => "svelte",
        }
    }
}

pub fn new_app(name: &str, dependency: &Dependency, stack: Stack) -> Plan {
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

    // These three differ between an API-only app and an Inertia one.
    match stack {
        Stack::Api => api_shell(&mut plan),
        Stack::Inertia(client) => inertia_shell(&mut plan, &crate_name, client),
    }

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

    plan.file(
        ".gitignore",
        match stack {
            Stack::Api => "/target\n/app.db\n/app.db-*\n.env\n".to_owned(),
            // `public/build` is generated by `npm run build`; committing it
            // means every deploy carries a stale bundle until someone rebuilds.
            Stack::Inertia(_) => {
                "/target\n/app.db\n/app.db-*\n.env\n/node_modules\n/public/build\n".to_owned()
            }
        },
    );

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
        match stack {
            Stack::Api => format!(
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
            Stack::Inertia(client) => format!(
                r#"# {crate_name}

Server-driven pages over the [Inertia.js](https://inertiajs.com) protocol, with
a {client} client. Controllers return components and props; there is no API
client, no client-side router and no CORS.

## Running it

Two terminals.

```sh
npm install
npm run dev               # Vite on http://localhost:5173
```

```sh
cargo luxid migrate       # apply migrations
cargo run                 # serve on http://127.0.0.1:3000
```

Open http://127.0.0.1:3000 — the Rust server, not Vite. Vite only serves the
JavaScript modules the page asks for, and hot-reloads them.

```sh
cargo luxid routes        # the routing table
cargo luxid openapi       # the OpenAPI 3.1 document (the /api group)
```

## How a page works

`src/controllers/pages_controller.rs` renders a component by name:

```rust
ctx.inertia("Home", json!({{ "app": "{crate_name}" }}))
```

`"Home"` resolves to `resources/js/Pages/Home.{page_ext}`, and the props arrive
as that component's props. A fresh browser load gets an HTML shell with the page
embedded in `data-page`; a link click gets JSON. The action does not know which.

## Forms and validation

Validation failures are **not** a 422 here. The Inertia middleware flashes the
errors to the session and redirects back, and they arrive on the next page as
the shared `errors` prop — the post-redirect-get pattern the client adapters
expect.

The `/api` group has no Inertia middleware, so the same action and the same
validator answer a JSON client with the ordinary 422 problem document.

## Deploying

```sh
npm run build             # writes public/build + its manifest
cargo build --release
```

A release build reads `public/build/.vite/manifest.json` for the hashed
filenames instead of talking to Vite, and `src/routes.rs` serves that directory
at `/build`. Bump the `.version(..)` in `src/routes.rs` on each deploy so open
tabs reload rather than requesting a bundle that no longer exists.

Generated by `luxid new --stack inertia --client {client}`.
"#,
                client = client.label(),
                page_ext = client.page_ext(),
            ),
        },
    );

    plan
}

/// `src/app.rs`, `src/routes.rs` and `src/controllers/mod.rs` for a JSON API.
fn api_shell(plan: &mut Plan) {
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
}

/// `src/app.rs`, `src/routes.rs`, `src/controllers/mod.rs` and the pages
/// controller for an Inertia application.
fn inertia_shell(plan: &mut Plan, crate_name: &str, client: Client) {
    let entry = client.entry();

    plan.file(
        "src/app.rs",
        r#"use std::sync::Arc;

use luxid::prelude::*;

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
    Providers::new()
        .singleton(move |_| db.clone())
        // Inertia is session-based: validation errors are flashed to the
        // session and read back after a redirect.
        //
        // `MemoryStore` keeps sessions in this process, so every restart signs
        // everybody out and a second instance shares nothing. Fine for
        // development; swap in a database- or cache-backed `SessionStore`
        // before running more than one process.
        .bind::<dyn SessionStore, _>(|_| Arc::new(MemoryStore::new()))
}
"#,
    );

    plan.file(
        "src/routes.rs",
        format!(
            r#"use luxid::prelude::*;

use crate::controllers;

pub fn register(r: &mut Router) {{
    // The built frontend. In development the Vite dev server serves these
    // files instead, so this route goes unused until `npm run build`.
    r.static_files("/build", "public/build");

    // ---- Inertia pages ------------------------------------------------
    //
    // Ordering is load-bearing. `Auth::session()` must be OUTSIDE `Inertia`:
    // the session guard writes back with `next.run(ctx).await?`, so it never
    // sees an `Err`. Inertia converts a validation failure into a redirect
    // before that happens, which is what lets the flashed errors survive.
    r.group("/", |r| {{
        r.middleware(Auth::session());
        r.middleware(
            Inertia::new("{entry}")
                .title("{crate_name}")
                // Bump on deploy so open tabs hard-reload instead of asking
                // for a bundle that no longer exists.
                .version(env!("CARGO_PKG_VERSION")),
        );

        r.get("/", controllers::pages_controller::PagesController::home);
    }});

    // ---- JSON API -----------------------------------------------------
    //
    // Unchanged by Inertia. The same action and the same validator can serve
    // both: a request here answers a validation failure with a 422 problem
    // document, because this group has no Inertia middleware.
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
        format!("pub mod health_controller;\npub mod pages_controller;\n\n{MARK_MODULES}\n"),
    );

    plan.file(
        "src/controllers/pages_controller.rs",
        format!(
            r#"use luxid::prelude::*;
use serde_json::json;

pub struct PagesController;

#[luxid::controller]
impl PagesController {{
    /// `GET /`
    ///
    /// `ctx.inertia(..)` serves both a fresh browser load (an HTML shell with
    /// the page embedded in `data-page`) and an Inertia navigation (JSON). The
    /// action does not know or care which — that is the whole point.
    ///
    /// The component name is a path under `resources/js/Pages`, so "Home"
    /// resolves to `resources/js/Pages/Home.{page_ext}`.
    async fn home(ctx: HttpContext) -> Result<Response> {{
        ctx.inertia("Home", json!({{ "app": "{crate_name}" }}))
    }}
}}
"#,
            page_ext = client.page_ext(),
        ),
    );

    frontend(plan, crate_name, client);
}

/// `package.json`, `vite.config.js`, the client entry and one page.
fn frontend(plan: &mut Plan, crate_name: &str, client: Client) {
    let entry = client.entry();
    let entry_ext = client.entry_ext();
    let page_ext = client.page_ext();

    // ---- package.json ----------------------------------------------------

    let (deps, dev_deps) = match client {
        Client::React => (
            r#""@inertiajs/react": "^2.0.0",
    "react": "^18.3.1",
    "react-dom": "^18.3.1""#,
            r#""@vitejs/plugin-react": "^4.3.4",
    "vite": "^6.0.7""#,
        ),
        Client::Vue => (
            r#""@inertiajs/vue3": "^2.0.0",
    "vue": "^3.5.13""#,
            r#""@vitejs/plugin-vue": "^5.2.1",
    "vite": "^6.0.7""#,
        ),
        Client::Svelte => (
            r#""@inertiajs/svelte": "^2.0.0",
    "svelte": "^5.16.0""#,
            r#""@sveltejs/vite-plugin-svelte": "^5.0.3",
    "vite": "^6.0.7""#,
        ),
    };

    plan.file(
        "package.json",
        format!(
            r#"{{
  "name": "{crate_name}",
  "private": true,
  "type": "module",
  "scripts": {{
    "dev": "vite",
    "build": "vite build"
  }},
  "dependencies": {{
    {deps}
  }},
  "devDependencies": {{
    {dev_deps}
  }}
}}
"#
        ),
    );

    // ---- vite.config.js --------------------------------------------------

    let (plugin_import, plugin_call) = match client {
        Client::React => ("import react from '@vitejs/plugin-react'", "react()"),
        Client::Vue => ("import vue from '@vitejs/plugin-vue'", "vue()"),
        Client::Svelte => (
            "import { svelte } from '@sveltejs/vite-plugin-svelte'",
            "svelte()",
        ),
    };

    plan.file(
        "vite.config.js",
        format!(
            r#"import {{ defineConfig }} from 'vite'
{plugin_import}

// The server reads `public/build/.vite/manifest.json` to find the hashed
// filenames, so `manifest`, `outDir` and `input` must stay in step with
// `Inertia::new(..)` in src/routes.rs.
export default defineConfig(({{ command }}) => ({{
  plugins: [{plugin_call}],

  // Only on build. In development Vite serves from its own root, and a base
  // here would break the dev-server URLs the HTML shell emits.
  base: command === 'build' ? '/build/' : '/',

  build: {{
    manifest: true,
    outDir: 'public/build',
    emptyOutDir: true,
    rollupOptions: {{ input: '{entry}' }},
  }},

  server: {{
    // Pinned: the shell hardcodes this origin in development. `strictPort`
    // makes a clash fail loudly rather than silently moving to 5174 and
    // serving a page whose scripts 404.
    port: 5173,
    strictPort: true,
  }},
}}))
"#
        ),
    );

    // ---- the client entry ------------------------------------------------

    let entry_source = match client {
        Client::React => format!(
            r#"import {{ createInertiaApp }} from '@inertiajs/react'
import {{ createRoot }} from 'react-dom/client'

import '../css/app.css'

// `createInertiaApp` reads the page object the server embedded in
// `<div id="app" data-page="...">`, resolves the named component, and takes
// over navigation from there. There is no client-side route table: the server
// decides which component renders, exactly as it would with templates.
createInertiaApp({{
  // Eagerly bundling every page keeps this scaffold simple. Swap `eager: true`
  // for a dynamic import once the app is big enough to want code splitting.
  resolve: (name) => {{
    const pages = import.meta.glob('./Pages/**/*.{page_ext}', {{ eager: true }})

    return pages[`./Pages/${{name}}.{page_ext}`]
  }},

  setup({{ el, App, props }}) {{
    createRoot(el).render(<App {{...props}} />)
  }},
}})
"#
        ),
        Client::Vue => format!(
            r#"import {{ createInertiaApp }} from '@inertiajs/vue3'
import {{ createApp, h }} from 'vue'

import '../css/app.css'

// `createInertiaApp` reads the page object the server embedded in
// `<div id="app" data-page="...">`, resolves the named component, and takes
// over navigation from there. There is no client-side route table: the server
// decides which component renders, exactly as it would with templates.
createInertiaApp({{
  // Eagerly bundling every page keeps this scaffold simple. Swap `eager: true`
  // for a dynamic import once the app is big enough to want code splitting.
  resolve: (name) => {{
    const pages = import.meta.glob('./Pages/**/*.{page_ext}', {{ eager: true }})

    return pages[`./Pages/${{name}}.{page_ext}`]
  }},

  setup({{ el, App, props, plugin }}) {{
    createApp({{ render: () => h(App, props) }})
      .use(plugin)
      .mount(el)
  }},
}})
"#
        ),
        Client::Svelte => format!(
            r#"import {{ createInertiaApp }} from '@inertiajs/svelte'
import {{ mount }} from 'svelte'

import '../css/app.css'

// `createInertiaApp` reads the page object the server embedded in
// `<div id="app" data-page="...">`, resolves the named component, and takes
// over navigation from there. There is no client-side route table: the server
// decides which component renders, exactly as it would with templates.
createInertiaApp({{
  // Eagerly bundling every page keeps this scaffold simple. Swap `eager: true`
  // for a dynamic import once the app is big enough to want code splitting.
  resolve: (name) => {{
    const pages = import.meta.glob('./Pages/**/*.{page_ext}', {{ eager: true }})

    return pages[`./Pages/${{name}}.{page_ext}`]
  }},

  setup({{ el, App, props }}) {{
    mount(App, {{ target: el, props }})
  }},
}})
"#
        ),
    };

    plan.file(format!("resources/js/app.{entry_ext}"), entry_source);

    // ---- one page --------------------------------------------------------

    let page = match client {
        Client::React => r#"// Props come from `ctx.inertia("Home", json!({ .. }))` in
// src/controllers/pages_controller.rs. No fetch, no loading state, no API
// client: the server rendered this page's data into the response that carried
// the component name.
export default function Home({ app, errors }) {
  return (
    <main>
      <h1>{app}</h1>
      <p>Inertia is working. Edit resources/js/Pages/Home.jsx.</p>

      {/* `errors` is a shared prop, present on every page. After a failed
          form submit the server redirects back and it arrives populated. */}
      {errors && Object.keys(errors).length > 0 && (
        <pre>{JSON.stringify(errors, null, 2)}</pre>
      )}
    </main>
  )
}
"#
        .to_owned(),
        Client::Vue => r#"<script setup>
// Props come from `ctx.inertia("Home", json!({ .. }))` in
// src/controllers/pages_controller.rs. No fetch, no loading state, no API
// client: the server rendered this page's data into the response that carried
// the component name.
defineProps({
  app: String,
  // A shared prop, present on every page. After a failed form submit the
  // server redirects back and it arrives populated.
  errors: { type: Object, default: () => ({}) },
})
</script>

<template>
  <main>
    <h1>{{ app }}</h1>
    <p>Inertia is working. Edit resources/js/Pages/Home.vue.</p>

    <pre v-if="Object.keys(errors).length">{{ errors }}</pre>
  </main>
</template>
"#
        .to_owned(),
        Client::Svelte => r#"<script>
  // Props come from `ctx.inertia("Home", json!({ .. }))` in
  // src/controllers/pages_controller.rs. No fetch, no loading state, no API
  // client: the server rendered this page's data into the response that
  // carried the component name.
  //
  // `errors` is a shared prop, present on every page. After a failed form
  // submit the server redirects back and it arrives populated.
  let { app, errors = {} } = $props()
</script>

<main>
  <h1>{app}</h1>
  <p>Inertia is working. Edit resources/js/Pages/Home.svelte.</p>

  {#if Object.keys(errors).length}
    <pre>{JSON.stringify(errors, null, 2)}</pre>
  {/if}
</main>
"#
        .to_owned(),
    };

    plan.file(format!("resources/js/Pages/Home.{page_ext}"), page);

    plan.file(
        "resources/css/app.css",
        r#":root {
  color-scheme: light dark;
}

body {
  margin: 0;
  padding: 2rem;
  font: 15px/1.6 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
}

main {
  max-width: 40rem;
  margin: 0 auto;
}
"#,
    );
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
        let plan = new_app("my-app", &Dependency::Version("0.2".into()), Stack::Api);

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

    /// Every file the plan claims to write must be distinct: `write` refuses
    /// to overwrite, so a duplicated path would fail at generation time.
    fn assert_unique_paths(plan: &Plan) {
        let mut seen = std::collections::BTreeSet::new();

        for file in &plan.files {
            assert!(
                seen.insert(file.path.clone()),
                "{} is planned twice",
                file.path.display()
            );
        }
    }

    #[test]
    fn an_api_app_has_no_frontend() {
        let plan = new_app("shop", &Dependency::Version("0.2".into()), Stack::Api);
        assert_unique_paths(&plan);

        let paths: Vec<_> = plan
            .touched()
            .iter()
            .map(|p| p.display().to_string())
            .collect();

        assert!(
            !paths.iter().any(|p| p.starts_with("resources/")),
            "{paths:?}"
        );
        assert!(!paths.contains(&"package.json".to_owned()), "{paths:?}");
        assert!(!paths.contains(&"vite.config.js".to_owned()), "{paths:?}");
    }

    #[test]
    fn each_client_scaffolds_its_own_entry_and_page() {
        for (client, entry, page) in [
            (
                Client::React,
                "resources/js/app.jsx",
                "resources/js/Pages/Home.jsx",
            ),
            (
                Client::Vue,
                "resources/js/app.js",
                "resources/js/Pages/Home.vue",
            ),
            (
                Client::Svelte,
                "resources/js/app.js",
                "resources/js/Pages/Home.svelte",
            ),
        ] {
            let plan = new_app(
                "shop",
                &Dependency::Version("0.2".into()),
                Stack::Inertia(client),
            );
            assert_unique_paths(&plan);

            let paths: Vec<_> = plan
                .touched()
                .iter()
                .map(|p| p.display().to_string())
                .collect();

            assert!(paths.contains(&entry.to_owned()), "{client:?}: {paths:?}");
            assert!(paths.contains(&page.to_owned()), "{client:?}: {paths:?}");
            assert!(paths.contains(&"package.json".to_owned()), "{client:?}");
        }
    }

    #[test]
    fn the_vite_input_matches_the_entry_the_server_looks_up() {
        // These two are a matched pair: Vite keys the manifest by its input
        // path, and the server looks the entry up by exactly that string. A
        // mismatch is a blank page in production and nowhere else.
        for client in [Client::React, Client::Vue, Client::Svelte] {
            let plan = new_app(
                "shop",
                &Dependency::Version("0.2".into()),
                Stack::Inertia(client),
            );

            let find = |path: &str| {
                plan.files
                    .iter()
                    .find(|file| file.path == Path::new(path))
                    .unwrap_or_else(|| panic!("{path} missing"))
                    .contents
                    .clone()
            };

            let entry = client.entry();

            assert!(
                find("vite.config.js").contains(&format!("input: '{entry}'")),
                "{client:?} vite input"
            );
            assert!(
                find("src/routes.rs").contains(&format!("Inertia::new(\"{entry}\")")),
                "{client:?} server entry"
            );
        }
    }

    #[test]
    fn an_inertia_app_keeps_the_api_group_and_its_marker() {
        // `make:model -c` inserts into <luxid:routes>; an Inertia app must
        // still offer somewhere for that to land.
        let plan = new_app(
            "shop",
            &Dependency::Version("0.2".into()),
            Stack::Inertia(Client::React),
        );

        let routes = plan
            .files
            .iter()
            .find(|file| file.path == Path::new("src/routes.rs"))
            .expect("routes.rs")
            .contents
            .clone();

        assert!(routes.contains(MARK_ROUTES), "{routes}");
        assert!(routes.contains("r.group(\"/api\""), "{routes}");
        assert!(
            routes.contains("static_files(\"/build\", \"public/build\")"),
            "{routes}"
        );
    }

    #[test]
    fn the_session_guard_wraps_the_inertia_middleware() {
        // Ordering is load-bearing: the session guard writes back with `?`, so
        // it must be outside the middleware that converts an Err into a
        // redirect, or flashed errors are dropped.
        let plan = new_app(
            "shop",
            &Dependency::Version("0.2".into()),
            Stack::Inertia(Client::React),
        );

        let routes = plan
            .files
            .iter()
            .find(|file| file.path == Path::new("src/routes.rs"))
            .expect("routes.rs")
            .contents
            .clone();

        let session = routes.find("Auth::session()").expect("session middleware");
        let inertia = routes.find("Inertia::new").expect("inertia middleware");

        assert!(
            session < inertia,
            "Auth::session() must be registered first"
        );
    }

    #[test]
    fn a_hyphenated_app_name_becomes_a_valid_crate_name() {
        let plan = new_app("my-app", &Dependency::Version("0.2".into()), Stack::Api);
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
