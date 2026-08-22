# 22 — Project: an Auth API

Everything so far, applied. We build a small API where people register, log in,
and read their own profile. Roughly forty minutes.

By the end you will have used: migrations, models, hooks, validation with a
database rule, JWT authentication, guards, and tests.

## 1. Create the project

```sh
luxid new authdemo
cd authdemo
```

Set a signing key. Copy the example file and edit it:

```sh
cp .env.example .env
```

```sh
# .env
DATABASE_URL=sqlite://./app.db?mode=rwc
APP_KEY=a-long-random-value-you-generate
LUXID_ADDR=127.0.0.1:3000
```

Generate one with `openssl rand -hex 32`. It never gets committed — `.env` is
gitignored.

## 2. Scaffold the user

```sh
luxid make:model User -a
```

Nine files, all registered for you.

One of them is a generic CRUD controller we do not want — this project writes its
own auth controller instead. Remove it:

```sh
rm src/controllers/users_controller.rs
```

and delete its line from `src/controllers/mod.rs`, and its
`r.resource("/users", ...)` line from `src/routes.rs`. (Leaving it there is not
harmless: it references an `UpdateUser` validator that step 5 replaces, so the
project will not compile.)

## 3. Describe the table

Open `migration/src/m<timestamp>_create_users.rs` and fill in the columns:

```rust
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    Name,
    Email,
    Password,
}

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260822_120000_create_users"      // keep whatever was generated
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Users::Table)
                    .if_not_exists()
                    .col(pk_auto(Users::Id))
                    .col(string(Users::Name))
                    .col(string_uniq(Users::Email))
                    .col(string(Users::Password))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.drop_table(Table::drop().table(Users::Table).to_owned()).await
    }
}
```

`string_uniq` adds a unique constraint. That is the database's guarantee;
chapter 15's `unique` rule is the *friendly* version that produces a `422`
instead of a `500`. You want both.

```sh
cargo run -- migrate
cargo run -- db:sync
```

`db:sync` fills the columns into `src/entities/users.rs` and
`src/factories/user_factory.rs`.

## 4. Never store a plaintext password

Open `src/entities/users.rs`. It now has your columns. Add a hook so hashing
cannot be skipped:

```rust
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, serde::Serialize, luxid::Model)]
#[luxid(before_create = Self::hash_password)]
#[sea_orm(table_name = "users")]
pub struct Model {
    // <luxid:fields>  refreshed by `cargo run -- db:sync`
    #[sea_orm(primary_key)]
    pub id: i64,
    pub name: String,
    pub email: String,
    #[serde(skip_serializing)]
    pub password: String,
    // </luxid:fields>
    #[sea_orm(ignore)]
    #[serde(flatten)]
    pub relations: luxid::Relations,
}

impl Model {
    async fn hash_password(active: &mut ActiveModel) -> luxid::Result<()> {
        if let sea_orm::ActiveValue::Set(password) = &active.password {
            active.password = sea_orm::ActiveValue::Set(luxid::Hash::make(password)?);
        }
        Ok(())
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
```

Two lines are doing security work:

- **`#[luxid(before_create = Self::hash_password)]`** — no code path can insert an
  unhashed password. Not a controller, not a seeder, not a test.
- **`#[serde(skip_serializing)]`** on `password` — the hash never appears in a
  JSON response, even if you return the whole user by accident.

`#[serde(skip_serializing)]` sits *outside* the markers, so `db:sync` will not
remove it. That is what the markers are for.

## 5. Validation rules

`src/validators/user.rs`:

```rust
use luxid::prelude::*;
use serde::Deserialize;

use crate::models::user::User;

#[derive(Debug, Deserialize, Validate, luxid::JsonSchema)]
pub struct StoreUser {
    #[validate(length(min = 2, max = 64))]
    pub name: String,

    #[validate(email, unique(User::email))]
    pub email: String,

    #[validate(length(min = 8))]
    pub password: String,
}

#[derive(Debug, Deserialize, Validate, luxid::JsonSchema)]
pub struct Credentials {
    #[validate(email)]
    pub email: String,

    #[validate(length(min = 1))]
    pub password: String,
}
```

`unique(User::email)` is the database-backed rule: registering with a taken
address gives a `422` saying *"has already been taken"*, not a constraint
violation.

Note `Credentials` does **not** check the password length. Login should not tell
an attacker your password policy, and a legacy password shorter than the current
minimum must still be able to log in.

## 6. The auth controller

Create `src/controllers/auth_controller.rs`:

```rust
use luxid::prelude::*;
use sea_orm::ActiveValue::Set;
use serde_json::json;

use crate::entities::users;
use crate::models::user::User;
use crate::validators::user::{Credentials, StoreUser};

pub struct AuthController;

#[luxid::controller]
impl AuthController {
    #[openapi(summary = "Register", tag = "auth", errors = [422])]
    async fn register(ctx: HttpContext) -> Result<Response> {
        let input = ctx.request.validate::<StoreUser>().await?;

        // The hook hashes this on the way in.
        let user = luxid::insert(users::ActiveModel {
            name: Set(input.name),
            email: Set(input.email),
            password: Set(input.password),
            ..Default::default()
        })
        .await?;

        let jwt = ctx.services.get::<Jwt>()?;
        let token = jwt.sign(&Identity::new(user.id.to_string()))?;

        ctx.response.created(json!({ "token": token, "user": user }))
    }

    #[openapi(summary = "Log in", tag = "auth", errors = [401, 422])]
    async fn login(ctx: HttpContext) -> Result<Response> {
        let input = ctx.request.validate::<Credentials>().await?;

        let found = User::query()
            .where_eq(User::email, input.email)
            .first()
            .await?;

        // One branch for both failures. A wrong email and a wrong password must
        // be indistinguishable, or this endpoint tells attackers which
        // addresses are registered.
        let Some(user) = found.filter(|u| Hash::verify(&input.password, &u.password)) else {
            return Err(Error::Unauthorized);
        };

        let jwt = ctx.services.get::<Jwt>()?;
        let token = jwt.sign(&Identity::new(user.id.to_string()))?;

        ctx.response.ok(json!({ "token": token }))
    }

    #[openapi(summary = "The current user", tag = "auth", errors = [401])]
    async fn me(ctx: HttpContext) -> Result<Response> {
        let user = User::find_or_fail(ctx.auth.id::<i64>()?).await?;

        ctx.response.ok(user)
    }
}
```

Declare the module in `src/controllers/mod.rs`:

```rust
pub mod auth_controller;
pub mod health_controller;
pub mod users_controller;

// <luxid:modules>
```

## 7. Register the signer

`src/app.rs`, in `providers`:

```rust
fn providers(db: Db, app_key: String) -> Providers {
    Providers::new()
        .singleton(move |_| db.clone())
        .singleton(move |_| Jwt::new(&app_key))
}
```

and read the key in `build`:

```rust
pub async fn build() -> luxid::Result<App> {
    let config = Config::load("luxid.toml")?;

    luxid::set_strict_relations(
        config.get_or("database.strict_relations", cfg!(debug_assertions))?,
    );

    let app_key: String = config.get("app.key")?;
    let url = config.get_or("database.url", "sqlite://./app.db?mode=rwc".to_owned())?;
    let db = Db::connect(url).await?;

    Ok(App::new()
        .config(config)
        .providers(providers(db, app_key))
        .middleware(WithDatabase)
        .routes(crate::routes::register))
}
```

`config.get` rather than `get_or`: an application with no signing key should
refuse to start, not run with a guessable one.

## 8. Routes

`src/routes.rs`:

```rust
use luxid::prelude::*;

use crate::controllers;

pub fn register(r: &mut Router) {
    r.group("/api", |r| {
        r.get("/health", controllers::health_controller::HealthController::show);

        r.post("/register", controllers::auth_controller::AuthController::register);
        r.post("/login", controllers::auth_controller::AuthController::login);

        r.group("/", |r| {
            r.middleware(Auth::jwt());

            r.get("/me", controllers::auth_controller::AuthController::me);
        });

        // <luxid:routes>
    });
}
```

Public routes above, guarded ones inside the group. Whether an endpoint needs a
token is visible at a glance.

## 9. Try it

```sh
cargo run
```

```sh
curl -X POST localhost:3000/api/register -H 'content-type: application/json' \
  -d '{"name":"Ada","email":"ada@example.com","password":"hunter2hunter2"}'
```

```json
{"token":"eyJ0eXAi...","user":{"id":1,"name":"Ada","email":"ada@example.com"}}
```

No `password` field — `skip_serializing` did that.

```sh
curl -X POST localhost:3000/api/register -H 'content-type: application/json' \
  -d '{"name":"A","email":"nope","password":"short"}'
```

```json
{
  "type": "https://luxid.rs/errors/validation",
  "title": "The given data was invalid",
  "status": 422,
  "errors": {
    "name": ["must be at least 2 characters"],
    "email": ["must be a valid email address"],
    "password": ["must be at least 8 characters"]
  }
}
```

Three problems, one response.

```sh
TOKEN=$(curl -s -X POST localhost:3000/api/login -H 'content-type: application/json' \
  -d '{"email":"ada@example.com","password":"hunter2hunter2"}' | jq -r .token)

curl -H "authorization: Bearer $TOKEN" localhost:3000/api/me
curl localhost:3000/api/me     # 401
```

And confirm registering the same address twice:

```json
{ "errors": { "email": ["has already been taken"] } }
```

## 10. Tests

`tests/auth.rs`:

```rust
use luxid::prelude::*;
use luxid_testing::TestApp;
use serde_json::json;

const SECRET: &str = "test-signing-key";

pub async fn database() -> Db {
    let db = Db::in_memory().await.expect("opens");
    db.migrate::<migration::Migrator>().await.expect("migrates");
    db
}

fn app(db: Db) -> TestApp {
    TestApp::new(
        App::new()
            .providers(
                Providers::new()
                    .singleton(move |_| db.clone())
                    .singleton(|_| Jwt::new(SECRET)),
            )
            .middleware(WithDatabase)
            .routes(authdemo::routes::register)
            .into_service(),
    )
}

fn registration() -> serde_json::Value {
    json!({ "name": "Ada", "email": "ada@example.com", "password": "hunter2hunter2" })
}

#[luxid::test(db = crate::database)]
async fn registering_returns_a_token_and_hides_the_password(db: Db) -> Result<()> {
    let response = app(db)
        .post("/api/register")
        .json(registration())
        .send()
        .await
        .assert_created()
        .assert_json_path("user.email", "ada@example.com");

    assert!(!response.body().contains("password"), "the hash must never be sent");
    assert!(response.json()["token"].is_string());

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn the_stored_password_is_hashed(db: Db) -> Result<()> {
    app(db).post("/api/register").json(registration()).send().await.assert_created();

    let user = authdemo::models::user::User::query()
        .where_eq(authdemo::models::user::User::email, "ada@example.com")
        .first_or_fail()
        .await?;

    assert_ne!(user.password, "hunter2hunter2");
    assert!(Hash::verify("hunter2hunter2", &user.password));

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn registering_twice_is_a_validation_error(db: Db) -> Result<()> {
    let app = app(db);

    app.post("/api/register").json(registration()).send().await.assert_created();

    app.post("/api/register")
        .json(registration())
        .send()
        .await
        .assert_validation_message("email", "has already been taken");

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn logging_in_and_reading_the_profile(db: Db) -> Result<()> {
    let app = app(db);

    app.post("/api/register").json(registration()).send().await.assert_created();

    let login = app
        .post("/api/login")
        .json(json!({ "email": "ada@example.com", "password": "hunter2hunter2" }))
        .send()
        .await
        .assert_ok();

    let token = login.json()["token"].as_str().expect("a token").to_owned();

    app.get("/api/me")
        .bearer(token)
        .send()
        .await
        .assert_ok()
        .assert_json_path("email", "ada@example.com");

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn a_wrong_password_and_an_unknown_email_look_the_same(db: Db) -> Result<()> {
    let app = app(db);

    app.post("/api/register").json(registration()).send().await.assert_created();

    let wrong = app
        .post("/api/login")
        .json(json!({ "email": "ada@example.com", "password": "wrongwrongwrong" }))
        .send()
        .await;

    let unknown = app
        .post("/api/login")
        .json(json!({ "email": "nobody@example.com", "password": "wrongwrongwrong" }))
        .send()
        .await;

    assert_eq!(wrong.status(), unknown.status());
    assert_eq!(wrong.json(), unknown.json());

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn the_profile_needs_a_token(db: Db) -> Result<()> {
    app(db).get("/api/me").send().await.assert_unauthorized();
    Ok(())
}
```

Tests reach your crate by name, so `authdemo::routes::register` refers to a
project created with `luxid new authdemo`. For that to work, the crate needs a
library target — add one alongside `main.rs`:

```rust
// src/lib.rs
pub mod app;
pub mod config;
pub mod controllers;
pub mod entities;
pub mod factories;
pub mod middleware;
pub mod models;
pub mod policies;
pub mod routes;
pub mod seeders;
pub mod services;
pub mod validators;
```

and change `src/main.rs` to use it:

```rust
#[tokio::main]
async fn main() -> luxid::Result<()> {
    let _ = dotenvy::dotenv();

    luxid::cli::run::<migration::Migrator>(authdemo::app::build().await?).await
}
```

Add the test harness:

```toml
[dev-dependencies]
luxid-testing = "0.1"
```

```sh
cargo test
```

Six tests, each in its own rolled-back transaction, running in parallel.

## What you built

- Registration and login with argon2-hashed passwords, enforced by a hook
- A `422` listing every problem at once, including a database-backed uniqueness
  check
- JWT issuing and a guarded route
- A login endpoint that does not leak which addresses are registered
- A test suite that leaves no rows behind

The two habits worth keeping: **hash in a hook, not a controller**, and **make
authentication failures indistinguishable**. Both are easy to get wrong by
writing the obvious code.

---

Previous: [21 — CLI Reference](21_CLI_Reference.md) · Next: [23 — Project: a Todo API](23_Project_Todo_App.md)
