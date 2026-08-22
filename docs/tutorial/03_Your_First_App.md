# 03 — Your First App

We will build one endpoint, understand every file involved, then add a second
endpoint that takes input. Keep this project — later chapters build on it.

```sh
luxid new blog
cd blog
```

## The tour

Nine things were created. Here is what each is for, in the order a request
touches them.

### `src/main.rs`

```rust
mod app;
mod config;
mod controllers;
// ... the rest of the module declarations

#[tokio::main]
async fn main() -> luxid::Result<()> {
    let _ = dotenvy::dotenv();

    luxid::cli::run::<migration::Migrator>(app::build().await?).await
}
```

Four lines of behaviour:

1. `dotenvy::dotenv()` loads a `.env` file if one exists. The `let _ =` means
   "it is fine if there isn't one".
2. `app::build()` assembles the application.
3. `luxid::cli::run` looks at the command-line arguments. No arguments means
   serve; `migrate`, `routes`, `openapi` and friends do those things instead.

`main.rs` rarely changes.

### `src/app.rs`

```rust
use luxid::prelude::*;

pub async fn build() -> luxid::Result<App> {
    let config = Config::load("luxid.toml")?;

    luxid::set_strict_relations(
        config.get_or("database.strict_relations", cfg!(debug_assertions))?,
    );

    let url = config.get_or("database.url", "sqlite://./app.db?mode=rwc".to_owned())?;
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
```

This is the one file that knows how the whole application fits together:
configuration is loaded, a database connection is opened, shared objects are
registered, global middleware is attached, routes are wired in.

Read it top to bottom whenever you forget how something is set up.

`WithDatabase` is middleware that makes the database available to every request.
Without it, queries fail with a message telling you it is missing.

### `src/routes.rs`

```rust
use luxid::prelude::*;

use crate::controllers;

pub fn register(r: &mut Router) {
    r.group("/api", |r| {
        r.get("/health", controllers::health_controller::HealthController::show);

        // <luxid:routes>
    });
}
```

The routing table, as plain code. `r.group("/api", ...)` puts everything inside
it under `/api`.

That `// <luxid:routes>` comment is a **marker**. When you run
`luxid make:model Post -c`, the generator inserts the new routes just above it.
Leave it there — but the lines it writes are ordinary code you own and can
rearrange.

### `src/controllers/health_controller.rs`

```rust
use luxid::prelude::*;
use serde_json::json;

pub struct HealthController;

#[luxid::controller]
impl HealthController {
    #[openapi(summary = "Liveness probe", tag = "system")]
    async fn show(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({ "status": "ok" }))
    }
}
```

The endpoint itself. Three things to notice:

- **`pub struct HealthController;`** — an empty struct that exists only to group
  related actions and give them a name.
- **`#[luxid::controller]`** — turns each `async fn` in the block into something
  the router can accept. Without it, `HealthController::show` would not exist as
  a route target.
- **`#[openapi(...)]`** — optional documentation, covered in chapter 19. Delete
  it and everything still works.

### The empty directories

`models/`, `entities/`, `validators/`, `services/`, `middleware/`, `policies/`,
`factories/`, `seeders/` each start with just a `mod.rs` containing a marker.
They fill up as you generate things. Each gets its own chapter.

### `migration/`

A separate small crate holding your database changes. Chapter 11.

### `luxid.toml`

```toml
[app]
name = "blog"
per_page = 20

[database]
strict_relations = true
```

Settings your application reads at startup and can read again from any action.
Environment variables override these — `app.name` is also `APP_NAME`. Chapter 10.

## Adding an endpoint

Create `src/controllers/greeting_controller.rs`:

```rust
use luxid::prelude::*;
use serde_json::json;

pub struct GreetingController;

#[luxid::controller]
impl GreetingController {
    async fn hello(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({ "message": "Hello from Luxid" }))
    }
}
```

Rust needs to be told the file exists. In `src/controllers/mod.rs`:

```rust
pub mod greeting_controller;
pub mod health_controller;

// <luxid:modules>
```

And register the route in `src/routes.rs`, inside the group:

```rust
r.get("/hello", controllers::greeting_controller::GreetingController::hello);
```

Run it:

```sh
cargo run
```

```sh
curl localhost:3000/api/hello
```

```json
{"message":"Hello from Luxid"}
```

**Three steps for every new controller**: write the file, declare the module,
register the route. Miss the second and you get "file not found in module tree";
miss the third and you get a 404.

## Reading input

Change the action to greet by name:

```rust
async fn hello(ctx: HttpContext) -> Result<Response> {
    let name: String = ctx.request.input("name")?.unwrap_or_else(|| "world".to_owned());

    ctx.response.ok(json!({ "message": format!("Hello, {name}") }))
}
```

```sh
curl 'localhost:3000/api/hello?name=Ada'
```

```json
{"message":"Hello, Ada"}
```

Three things are happening in that one line:

- **`input`** looks in the query string first, then the JSON body. `?name=Ada`
  and `{"name":"Ada"}` both work.
- **`Option`** — the key might be absent, so you decide the default.
- **`?`** — the value might be present but undecodable. Ask for a `u32` and send
  `?name=abc` and the client gets a `400` explaining which field failed. You did
  not write that handling.

Try it:

```sh
curl 'localhost:3000/api/hello?name=Ada&name=Grace'   # first one wins
```

## Seeing your routes

```sh
cargo run -- routes
```

```
GET  /api/health  HealthController::show      [1 middleware]
GET  /api/hello   GreetingController::hello   [1 middleware]
```

Every registered route, what handles it, and how many middleware wrap it. When
an endpoint 404s, this is the first thing to check — usually the route was never
registered, or the path differs from what you are requesting.

## What you now know

- How a request finds its way from `routes.rs` to an action
- The three steps for adding a controller
- That `ctx.request.input` reads from the query string or body, and that `?`
  turns bad input into a proper error response
- That `cargo run -- routes` answers "why is this 404ing?"

---

Previous: [02 — Installation](02_Installation.md) · Next: [04 — Routing](04_Routing.md)
