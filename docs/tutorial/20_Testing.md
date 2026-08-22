# 20 — Testing

Luxid tests go through the **real** service: the same routing, middleware,
container, and adapter that production uses. A passing test therefore means the
endpoint works, not that a parallel code path works.

## The shape

```rust
// tests/posts.rs
use luxid::prelude::*;
use luxid_testing::TestApp;
use serde_json::json;

#[luxid::test(db = crate::support::database)]
async fn the_index_is_paginated(db: Db) -> Result<()> {
    app(db)
        .get("/api/posts")
        .send()
        .await
        .assert_ok()
        .assert_json_count("data", 2)
        .assert_json_path("data.0.title", "First");

    Ok(())
}
```

Add the harness to your `Cargo.toml`:

```toml
[dev-dependencies]
luxid-testing = "0.1"
```

## Each test gets a clean database

`#[luxid::test(db = ...)]` runs the body inside a **transaction that is rolled
back afterwards**. Tests share one database, run in parallel, and need no
truncation, no fixtures, and no ordering.

The `db = ` argument names a function returning a `Db`:

```rust
// tests/support.rs — or a module in your test file
use luxid::prelude::*;

pub async fn database() -> Db {
    let db = Db::in_memory().await.expect("opens");
    db.migrate::<migration::Migrator>().await.expect("migrates");
    db
}
```

`Db::in_memory()` gives an isolated SQLite database. Running your real
migrations against it means your tests exercise the real schema.

Without a database argument, `#[luxid::test]` is `#[tokio::test]` plus `Result`
unwrapping.

## Building the app under test

```rust
fn app(db: Db) -> TestApp {
    TestApp::new(
        App::new()
            .providers(
                Providers::new()
                    .singleton(move |_| db.clone())
                    .singleton(|_| Jwt::new(SECRET)),
            )
            .middleware(WithDatabase)
            .routes(crate::routes::register)
            .into_service(),
    )
}
```

Note it registers `crate::routes::register` — the **real** routing table. Tests
that wire up their own routes test their own wiring rather than yours.

`into_service()` deliberately skips the boot-time check that every singleton
resolves, so a test can bind only what it needs.

## Making requests

```rust
app.get("/api/posts").send().await;

app.post("/api/posts")
    .json(json!({ "title": "Hello" }))
    .send()
    .await;

app.put("/api/posts/1").json(body).send().await;
app.delete("/api/posts/1").send().await;

app.get("/api/me").header("x-trace", "abc").send().await;
app.get("/api/me").bearer(token).send().await;
```

### Acting as a user

```rust
app(db).get("/api/me").acting_as(SECRET, user.id).send().await.assert_ok();
```

`acting_as` signs a real token with your secret, so the request goes **through**
the guard rather than around it. A test that bypassed the guard would not be
testing the guard.

With claims:

```rust
app.get("/api/admin")
    .acting_as_with(SECRET, user.id, [("role".to_owned(), json!("admin"))])
    .send()
    .await;
```

And for sessions:

```rust
app.get("/api/cart").with_session(session_id).send().await;
```

## Assertions

```rust
.assert_ok()                    // 200
.assert_created()               // 201
.assert_no_content()            // 204
.assert_unauthorized()          // 401
.assert_forbidden()             // 403
.assert_not_found()             // 404
.assert_status(418)

.assert_header("content-type", "application/json; charset=utf-8")

.assert_json_path("data.0.title", "First")
.assert_json_count("data", 3)
.assert_validation_message("email", "has already been taken")
.assert_validation_errors(&["email", "name"])
```

They chain, and **every failure prints the response body** — a failure that says
only "expected 200, got 500" costs a debugging session the body would have
saved.

`assert_validation_errors` asserts a `422` naming **exactly** those fields;
extra or missing fields both fail. That is usually what you want, since a rule
firing that you did not expect is a bug.

For anything else, read the body:

```rust
let response = app.get("/api/posts").send().await;
let body = response.json();

assert_eq!(body["data"].as_array().unwrap().len(), 2);
```

Note `assert_json_path` reads from the **root** of the body. Validation errors
live under `errors`, so it is `errors.email.0` — or just use
`assert_validation_message`, which exists so nobody gets that prefix wrong.

## Factories

A factory describes a *typical* row so tests can override only what they care
about:

```rust
use luxid::prelude::*;
use sea_orm::ActiveValue::Set;

use crate::entities::users;

pub struct UserFactory;

impl Factory for UserFactory {
    type Active = users::ActiveModel;

    fn definition() -> Self::Active {
        let n = next_id();

        users::ActiveModel {
            name: Set(format!("User {n}")),
            email: Set(format!("user{n}@example.com")),
            role: Set("member".to_owned()),
            ..Default::default()
        }
    }
}
```

```rust
UserFactory::new().create_one().await?;                                  // one
UserFactory::new().count(3).create().await?;                             // three
UserFactory::new().state(|u| u.role = Set("admin".into())).create_one().await?;
UserFactory::new().count(2).make();                                      // no database
```

Make each generated row **distinct** — a counter, a random suffix. Three
identical rows break any test that asserts on a unique column.

States apply in order, so a later one wins. `create_one` ignores `count`.

`luxid make:model User -f` generates the file; `cargo run -- db:sync` fills in
the required columns from your schema.

## What to test

Endpoints, mostly — the thing a client actually touches:

```rust
#[luxid::test(db = crate::support::database)]
async fn only_the_owner_may_update(db: Db) -> Result<()> {
    let owner = UserFactory::new().create_one().await?;
    let other = UserFactory::new().create_one().await?;
    let post = PostFactory::new()
        .state(move |p| p.user_id = Set(owner.id))
        .create_one()
        .await?;

    let app = app(db);

    app.put(&format!("/api/posts/{}", post.id))
        .acting_as(SECRET, owner.id)
        .json(json!({ "title": "Updated" }))
        .send()
        .await
        .assert_ok();

    app.put(&format!("/api/posts/{}", post.id))
        .acting_as(SECRET, other.id)
        .json(json!({ "title": "Hijacked" }))
        .send()
        .await
        .assert_forbidden();

    Ok(())
}
```

Policies, scopes, and pure helpers are worth unit-testing directly since they
need no HTTP.

## Turn N+1s into failures

Leave strict relations on in tests:

```toml
[database]
strict_relations = true
```

Then an endpoint that forgets `.with("author")` fails its test rather than
quietly issuing a query per row.

## Running

```sh
cargo test                     # everything
cargo test --test posts        # one file
cargo test only_the_owner      # by name
cargo test -- --nocapture      # show println output
```

---

Previous: [19 — OpenAPI](19_OpenAPI.md) · Next: [21 — CLI Reference](21_CLI_Reference.md)
