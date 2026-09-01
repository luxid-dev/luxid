# Luxid — Design

**Status:** Living document — updated as the implementation proves or
disproves what it says. Started 2026-08-22; last checked against the code at
0.3.0, where §17 records what is built and what this document still only
specifies.
**Substrate:** salvo · SeaORM 2.0 · Rust edition 2024 (MSRV 1.94, developed on 1.97.1)

---

## 1. What Luxid is

A convention-over-configuration web framework for Rust, modelled on Laravel by way of
**AdonisJS** — specifically Adonis's `HttpContext` controller ergonomics, which survive
static typing better than Laravel's reflection-driven container does.

Built on **salvo**, which is sealed inside the framework: salvo types never appear in a
public Luxid signature.

**Audience:** developers who want Laravel/Adonis productivity with Rust's deployment and
performance characteristics. Explicitly a public open-source project, not an internal tool.

### Goals

- An app is productive within minutes of `luxid new`, with no infrastructure beyond Postgres.
- Controllers read like Adonis controllers.
- Errors, validation, and 404s require no ceremony in the action body.
- Rebuild-to-restart stays under 3 seconds on a warm cache.
- OpenAPI is generated, not hand-maintained.

### Non-goals for 1.0

- Server-rendered views and an asset pipeline (architecture stays SSR-ready; feature deferred).
- Supporting multiple ORMs. SeaORM is the one data layer.
- A GraphQL layer.
- Facades / global service accessors (see §8, deliberate omission).

### Positioning against loco-rs

loco is Rails-shaped on axum. Luxid is Laravel/Adonis-shaped on salvo. The concrete
differentiators, in the order a stranger would notice them:

1. `HttpContext` controllers instead of extractor-based handlers — no extractor trait-bound errors.
2. Async DB-backed validation rules (`unique`, `exists`) — no Rust framework ships these.
3. First-class Inertia.js adapter — the migration path for Laravel refugees.
4. Postgres-backed queues, no Redis (0.2).
5. Compile time treated as a tracked feature, not an accepted tax.

---

## 2. Workspace architecture

One workspace, one version number, all crates published in lockstep. Semver tractability
matters more than crate independence for a framework.

| Crate | Contents |
|---|---|
| `luxid` | Facade. The only dependency an app declares. Re-exports, feature-gated. |
| `luxid-core` | `HttpContext`, `Request`, `Response`, `Error`, router, middleware, container, **the entire salvo adapter** |
| `luxid-macros` | `#[controller]`, `#[openapi]`, `#[middleware]`, `#[model]`, `derive(Model)`, `derive(Validate)`, `#[test]` |
| `luxid-orm` | The data layer over SeaORM: the `Record` trait, query builder, relations, pagination, hooks, scopes |
| `luxid-cli` | The `luxid` binary |
| `luxid-testing` | `TestApp`, factories, transaction-rollback harness |

Salvo appears in exactly one crate. `luxid::raw` re-exports the underlying salvo types
behind a feature flag as the power-user escape hatch.

### Generated app skeleton

Mirrors the layout already arrived at independently in `rs/salvo/slinttech-server`.

```
my-app/
├── luxid.toml            # framework config: paths, generators, openapi, strict_relations
├── .env / .env.example
├── .cargo/config.toml    # linker + debuginfo tuning (§12)
├── migration/            # separate crate, SeaORM convention
└── src/
    ├── main.rs           # visible, ~10 lines
    ├── app.rs            # providers, global middleware, exception handler
    ├── routes.rs         # explicit route table; CLI appends, humans read
    ├── controllers/
    ├── entities/         # sea-orm-cli output — never hand-edited
    ├── models/           # your models — yours, never clobbered
    ├── validators/       # StoreUser / UpdateUser
    ├── services/  middleware/  policies/  config/
└── tests/
```

`entities/` and `models/` are split precisely so regenerating entities after a migration
cannot destroy model behaviour.

### Boot

Explicit and readable. No auto-discovery, no `inventory`/`linkme` linker tricks — a route
that does not register must be visible in ordinary code.

```rust
// src/main.rs
#[tokio::main]
async fn main() -> luxid::Result<()> {
    luxid::App::new(config::app())
        .providers(app::providers())
        .middleware(app::middleware())
        .routes(routes::register)
        .run().await
}

// src/routes.rs
pub fn register(r: &mut Router) {
    r.group("/api/v1", |r| {
        r.resource("/users", UsersController).middleware(Auth::jwt());
        r.post("/auth/login", AuthController::login);
    });
}
```

---

## 3. Controllers

`#[luxid::controller]` on an inherent impl block. Every action takes an owned
`HttpContext` and returns `Result<Response>`.

```rust
#[luxid::controller]
impl UsersController {
    #[openapi(ok = Paginated<User>)]
    async fn index(ctx: HttpContext) -> Result<Response> {
        let page  = ctx.request.input::<u32>("page")?.unwrap_or(1);
        let me    = ctx.auth.user().await?;
        let users = User::query().active().where_eq(User::team_id, me.team_id).paginate(page, 20).await?;
        ctx.response.ok(users)
    }

    #[openapi(body = StoreUser, created = User, errors = [422, 409])]
    async fn store(ctx: HttpContext) -> Result<Response> {
        let HttpContext { request, response, .. } = ctx;
        let input = request.validate::<StoreUser>().await?;
        response.created(User::create(input).await?)
    }
}
```

### Why owned, and why no lifetime parameter

Verified on the target toolchain: **edition 2024 makes elided lifetimes in paths a hard
error (E0726)**. A borrowing context would force `ctx: HttpContext<'_>` in every action.
To get `ctx: HttpContext`, the context must own its parts.

This is a benefit, not merely a cost: an owning context is what lets salvo be sealed off
entirely. The generated handler takes salvo's borrowed request, builds a Luxid
`HttpContext`, runs the action, and writes the returned `Response` back.

### House style

`ctx: HttpContext` in signatures; destructure inside the body when preferred.

Both signature-position destructuring (`HttpContext { request, response, .. }: HttpContext`)
and body destructuring (`let HttpContext { request, .. } = ctx;`) are verified to work
across a crate boundary. Partial moves also work — a field can be handed to a helper while
other fields stay in use.

The default is the short signature purely on formatting grounds: 57 columns versus 92, and
92 makes rustfmt split the signature in every doc sample and every generated file.

### Forward compatibility

`HttpContext` is `#[non_exhaustive]`. Verified: downstream crates are **required** to write
`..` in struct patterns (E0638). Adding fields post-1.0 — `inertia`, `cache`, `session` —
can therefore never be a breaking change.

---

## 4. HttpContext

```rust
#[non_exhaustive]
pub struct HttpContext {
    pub request:    Request,      // owned
    pub response:   Response,     // owned, consumed on terminal methods
    pub params:     Params,
    pub extensions: Extensions,   // typed, request-scoped bag
    pub services:   Container,
    pub auth:       Auth,         // anonymous unless a guard set it
    pub config:     Config,
    pub session:    Session,      // detached unless `Auth::session()` is active
}
```

There is deliberately **no `db` field**. The connection is ambient for the
duration of a request — `WithDatabase` puts it in scope and queries pick it up —
which is what lets model code read `Post::find(id).await?` rather than
`Post::find(&db, id).await?`. Where the handle itself is wanted, for a
transaction, it resolves like any other service: `ctx.services.get::<Db>()?`.

`auth` is present on every route and carries an *identity*, not a user record.
`auth.identity()` returns `Result<&Identity>` (401 if anonymous),
`auth.try_identity()` returns `Option<&Identity>`, `auth.id::<T>()` parses the
subject, and `auth.check()` is the bare question. Loading the row is one line in
the action: `User::find_or_fail(ctx.auth.id::<i64>()?).await?`.

**Request:** `input::<T>` (query first, then body), `query`, `query_all`,
`body_json::<T>`, `body_bytes`, `header`, `headers`, `cookie`, `bearer_token`,
`method`, `uri`, `path`, `validate::<T>`.

**Response:** builder methods return `Response`; terminal methods return `Result<Response>`.

```rust
response.ok(body)                                   response.no_content()
response.created(body)                              response.text(s)
response.accepted(body)                             response.bytes(data, mime)
response.status(418).header("x-trace", id).json(v)  response.redirect(url)   // 303
```

> **Not built.** The `events` and `logger` fields and the `ctx.cache()` /
> `ctx.mail()` / `ctx.queue()` accessors this section once specified do not
> exist; the services behind them sit on the §14 roadmap, and
> `#[non_exhaustive]` is what keeps adding them non-breaking. Request helpers
> for uploads (`file`, `files`) and the peer address (`ip`) are likewise
> unbuilt.

---

## 5. Errors

One error type; every variant carries an HTTP mapping; `From` impls for everything an
action would `?` on.

```rust
pub enum Error {
    Validation(ValidationErrors),   // 422
    NotFound { resource, id },      // 404
    Unauthorized,                   // 401
    Forbidden,                      // 403
    Conflict(String),               // 409
    TooManyRequests,                // 429
    BadRequest(String),             // 400
    Http { status, code, message, details },
    Internal(anyhow::Error),        // 500 — logged in full, redacted in the response
}

pub type Result<T> = std::result::Result<T, Error>;
```

This is what keeps actions short: `User::find_or_fail(id).await?` yields a clean 404 with
no handling in the body.

There is no separate database variant: a `DbErr` becomes `Internal` carrying the
underlying message, so it is logged in full and redacted in the response like any
other 500.

Rendering happens in the salvo adapter, which is the one seam between Luxid and
the substrate. Output is RFC 7807 `problem+json`:

```json
{ "type": "https://luxid.rs/errors/validation",
  "title": "the given data was invalid",
  "status": 422,
  "errors": { "email": ["must be a valid email address"] } }
```

RFC 7807 is chosen because it yields free OpenAPI error schemas and free client codegen.

Lifecycle: `HttpContext built → middleware → action → Result<Response> → problem
document (on Err) → salvo response`.

> **Not built.** An app-owned exception handler generated into `src/app.rs`,
> equivalent to Laravel's `Handler.php`, was specified here and is not
> implemented. Rendering is currently not overridable from the application.

---

## 6. Validation

`derive(Validate)` expands synchronous rules inline — no `validator` crate
dependency, the rules are Luxid's own — and adds **asynchronous, DB-backed
rules**, the feature most conspicuously missing from Rust validation crates.

```rust
#[derive(Validate, Deserialize)]
pub struct StoreUser {
    #[validate(length(min = 2, max = 64))]     pub name: String,
    #[validate(email, unique(User::email))]    pub email: String,   // async
    #[validate(length(min = 8))]               pub password: String,
    #[validate(exists(Team::id))]              pub team_id: i64,    // async
}
```

`request.validate::<StoreUser>().await?` runs sync rules first, then the async
rules in a single pass against the ambient connection, **skipping any field that
already failed**, and aggregates into one 422. Never a per-field round trip,
never a partial report, and never two messages for one mistake.

`unique` and `exists` correspond to Laravel's `unique:users,email` and `exists:teams,id`.

---

## 7. Middleware

Same `HttpContext` type as controllers — one mental model framework-wide.

```rust
#[luxid::middleware]
impl AuthMiddleware {
    async fn handle(&self, mut ctx: HttpContext, next: Next) -> Result<Response> {
        let jwt = ctx.services.get::<Jwt>()?;
        let token = ctx.request.bearer_token().ok_or(Error::Unauthorized)?;
        ctx.auth.set(jwt.verify(token)?);

        let res = next.run(ctx).await?;
        Ok(res.header("x-authenticated", "1"))
    }
}
```

Code above `next.run()` runs before; below runs after; early return short-circuits. No
separate before/after API.

`handle` takes `&self` so configured middleware can hold state — which is how
`Auth::jwt()` and `RequireHeader::new("x-signature")` are ordinary values rather
than special cases.

Attachment is **typed, not string-keyed**, so typos are compile errors rather than runtime
surprises. `middleware` returns the route, so several chain:

```rust
r.resource("/admin", AdminController)
    .middleware(Auth::jwt())
    .middleware(Role::admin());
```

Order is global → group → route. `cargo luxid routes` prints how many middleware
wrap each route, so a route missing its guard shows a lower count than its
neighbours.

---

## 8. Service container

ASP.NET Core's lifetime model — the proven static-language answer.

```rust
// src/app.rs
pub fn providers(db: Db) -> Providers {
    Providers::new()
        .singleton(move |_| db.clone())
        .singleton(|c| UserService::new(c.get::<Db>().expect("Db is registered")))
        .scoped(|_| RequestId::new())
        .transient(|_| Formatter::new())
        .bind::<dyn Mailer, _>(|_| Arc::new(Smtp::new()))
}
```

Resolution: `ctx.services.get::<UserService>()?` for a concrete type,
`ctx.services.get_dyn::<dyn Mailer>()?` for a bound trait. `try_singleton` is
the variant whose factory may fail, so a pool that cannot connect fails at boot
rather than on first use.

Runtime resolution is the trade already accepted with pure-context controllers. Blast radius is
narrowed two ways:

1. **`App::run()` eagerly resolves every singleton at boot.** A missing or cyclic binding
   fails at startup, naming the type and the provider line it needs — not on first request
   in production.
2. **`luxid check`** statically scans for `services.get::<T>()` with no matching binding and
   reports a warning. Honestly labelled best-effort: it cannot see through generics.

### Deliberate omission: no facades

Laravel's `Cache::get()` works because PHP resolves through a global container. The Rust
equivalent requires global mutable state, which damages testability, parallel test
execution, and multi-app processes. Resolving through the context —
`ctx.services.get::<T>()?` — reads nearly as short with none of the globals.

This is a visible, intentional divergence from Laravel and should be documented as one.

---

## 9. The data layer

### Models

`sea-orm-cli` owns `entities/`. Luxid adds a derive and a relations bag. Verified against
sea-orm-macros 2.0.2: `#[sea_orm(ignore)]` permits non-column fields on a model, so no
wrapper type is needed.

```rust
// src/entities/users.rs — generated
#[derive(Clone, Debug, DeriveEntityModel, Serialize, luxid::Model)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]     pub id: i64,
    #[sea_orm(unique)]          pub email: String,
    #[serde(skip_serializing)]  pub password: String,
    pub team_id: i64,
    pub deleted_at: Option<DateTimeUtc>,

    #[sea_orm(ignore)] #[serde(flatten)] pub relations: Relations,
}
```

```rust
// src/models/user.rs — yours
pub use crate::entities::users::Model as User;

#[luxid::model(
    has_many(posts = Post, fk = "user_id"),
    belongs_to(team = Team),
)]
impl User {
    #[scope]
    fn active(query: Query<users::Entity>) -> Query<users::Entity> {
        query.where_null(User::deleted_at)
    }
}
```

Relations generate a typed accessor each — `user.posts()`, `user.team()` — so
two relations pointing at the same model stay unambiguous.

**Relations are declared in the attribute, not as `#[has_many] fn posts();`.**
A bodiless `fn` is not parseable Rust: syn rejects it before the macro runs, so
that spelling would greet users with `expected `{`` pointing inside a macro they
did not write. Scopes keep the `#[scope]` spelling because they have bodies.

Hooks are named on the derive, and their functions live in a plain `impl`:

```rust
#[derive(.., luxid::Model)]
#[luxid(before_create = Self::hash_password, after_create = Self::send_welcome)]
#[sea_orm(table_name = "users")]
pub struct Model { .. }

impl Model {
    async fn hash_password(active: &mut users::ActiveModel) -> Result<()> {
        if let Set(password) = &active.password {
            active.password = Set(Hash::make(password)?);
        }
        Ok(())
    }
}
```

**Why not `#[before_save]` inside the model block?** `luxid::insert` and
`luxid::update` *require* the hooks trait, so hooks always run on the ordinary
write path — a hook that silently fails to fire is a security bug, not an
inconvenience. Requiring the trait means every model implements it, which means
the derive generates it, which means the derive must know the hooks. Declaring
them elsewhere would leave hookless models uninsertable or produce conflicting
impls. `insert_without_hooks` is the escape hatch, named for what it costs.

Order: `before_save` → `before_create` → write → `after_create` → `after_save`,
mirrored for updates. An `Err` from any *before* hook aborts the write.

### Queries

```rust
User::find(id).await?              // Option<User>
User::find_or_fail(id).await?      // User, else Error::NotFound → 404

User::query()
    .active()
    .where_eq(User::team_id, team_id)
    .with("posts")
    .order_by_desc(User::created_at)
    .paginate(page, 20).await?     // Paginated<User>

// Writes take a SeaORM ActiveModel and run the model's hooks.
luxid::insert(users::ActiveModel { name: Set(name), ..Default::default() }).await?;
luxid::update(active).await?;
luxid::delete_by_id::<users::Entity>(id).await?;   // bool: was anything removed
```

### Typed columns

`User::team_id` is a generated zero-sized type carrying `Value = i64`. `.where_eq(User::team_id, "abc")`
fails to compile, where SeaORM's `users::Column::TeamId.eq("abc")` compiles and fails at runtime.

### Eager loading

Runtime-typed, by name: `.with("posts")`, read back through the method the
relation generates — `user.posts()?` yields `&[Post]`, `post.author()?` yields
`Option<&User>`. Naming the method after the relation is what keeps two
relations onto the same model unambiguous. Loading is batched: one query per
relation whatever the page size, with duplicate keys collapsed first.

Consistent with the pure-context trade — runtime typing where static typing
would cost more ergonomics than it is worth. A name that matches no declared
relation is an error listing the ones that exist.

Loaded relations serialize inline with the model's own columns, and a model with
nothing loaded renders no relation keys at all.

> **Single-level only.** `.with("posts.comments")` is not implemented and
> reports the path as an undeclared relation. Arbitrary-depth graphs were
> specified here; load one level and query the second.

### Strict relations

`strict_relations = true` in `luxid.toml`, default **on in dev, off in prod**. Accessing an
unloaded relation raises an error naming the missing `.with()` path. Turns N+1 from a
production surprise into a failing test.

### Transactions

Explicit, on the handle resolved from the container:

```rust
let db = ctx.services.get::<Db>()?;

db.transaction(async || {
    let user = luxid::insert(new_user).await?;
    luxid::insert(new_profile(user.id)).await?;
    Ok(())
}).await?;
```

Commits on `Ok`, rolls back on `Err`. Every query inside joins the transaction
through the same ambient handle, so there is no `tx` to thread through.

---

## 10. OpenAPI

Attribute-driven, adjacent to the action it describes:

```rust
#[openapi(body = StoreUser, created = User, errors = [422, 409])]
```

Pure-context signatures carry no type information, so inference is not available. The attribute
restates types the body also mentions; `luxid check` lints for drift between the two.

`luxid openapi` emits the spec by walking the **route table**, not a static
registry: a registry would mean `inventory`/`linkme` linker tricks, which §2
ruled out for routing. Routes are registered explicitly, so `Action::openapi()`
hangs the metadata on the action and the document is assembled from the routes
that actually exist. A route missing from the spec is a route missing from
`luxid routes`.

The document is OpenAPI **3.1**, which *is* JSON Schema, so `schemars` output
drops in without translation. Error statuses reference an RFC 7807 `Problem`
component automatically, and path parameters are derived from the route pattern
so they cannot disagree with it. Undocumented actions appear marked
`Undocumented` rather than being omitted.

---

## 11. Auth

Two guards behind one interface — API-first, SSR-ready.

```rust
r.resource("/users", UsersController).middleware(Auth::jwt());
r.resource("/admin", AdminController).middleware(Auth::session());

ctx.auth.id::<i64>()?                       // subject, parsed; 401 if anonymous
ctx.auth.try_identity()                     // Option<&Identity>, never fails
ctx.authorize(PostPolicy::update, &post)?   // → Error::Forbidden → 403
ctx.can(PostPolicy::update, &post)          // bool, no consequence
```

`Hash` wraps argon2id; tokens use `jsonwebtoken`, with a one-hour default TTL.
Session cookies default to fourteen days, carry an opaque 256-bit id and nothing
else, and rotate that id on `login` so a fixed session id is worthless after
authentication.

A guard is ordinary middleware rather than an implementation of a `Guard`
trait — `Auth::jwt()` returns a configured `JwtGuard` that implements
`Middleware` — so an API-key or OAuth guard needs no Luxid release. Anything
that sets `ctx.auth` before calling `next` is a guard.

---

## 12. CLI and the developer loop

### Commands

`make:model` is the **only** generator. Non-model files are written by hand.

```
luxid make:model User        model
luxid make:model User -m     + migration
luxid make:model User -mc    + resource controller
luxid make:model User -mfsc  + factory + seeder + controller
luxid make:model User -a     model, migration, factory, seeder, policy,
                             resource controller, form requests

  -m migration   -f factory   -s seeder   -c resource controller   -a all
```

`-c` generates an **API** resource controller (`index show store update destroy`;
no `create`/`edit`) and appends its routes to `routes.rs`.

### The command line is two binaries, not one

Runtime commands operate on the route table, the migration list and the
container — all of them types in the *application's* crate. No external binary
can see them. So:

* **`luxid`** — standalone, installed once, filesystem only: `new`,
  `make:model`.
* **The app's own binary** — `cargo luxid serve | migrate | migrate:rollback |
  migrate:fresh | migrate:status | db:sync | routes | openapi`, wired up by one
  line in `main.rs`:

  ```rust
  luxid::cli::run::<migration::Migrator>(app::build().await?).await
  ```

This is the split loco uses, for the same reason. `migrate:fresh` requires
`--force`, because dropping every table should not follow from a mistyped
command in the wrong shell.

Registration uses markers in the generated files, and inserts *above* them so
repeated generation stays chronological. Writing refuses if any target file
exists and writes nothing in that case: a half-applied generator is worse than
one that declined.

The linker tuning in `.cargo/config.toml` ships **commented out**. Enabling mold
by default produces a project that does not compile on a machine without it —
and that file is committed, so it would break teammates too. `luxid new` prints
a hint only when mold is actually on `PATH`.

### No `--fields`

Deliberately absent. SeaORM entities are derived from the live schema; a field DSL would be
a second, weaker source of truth that cannot express every column type. Migrations are
generated as empty stubs, as in Laravel.

### Marked regions close Laravel's stub gap

Because factories and form requests are generated before the schema exists, they start
empty. `cargo luxid db:sync` reads the live schema and refreshes the mechanical
portion inside marked regions while preserving everything written outside them:

```rust
// src/entities/users.rs
pub struct Model {
    // <luxid:fields>  refreshed by `cargo luxid db:sync`
    #[sea_orm(primary_key)]     pub id: i64,
    pub name: String,
    #[serde(skip_serializing)]  pub password: String,   // attribute carried over
    // </luxid:fields>

    #[sea_orm(ignore)] #[serde(flatten)] pub relations: Relations,   // preserved
}
```

Attributes a field already carries are re-attached on regeneration, because
`#[serde(skip_serializing)]` on a password hash silently becoming "sent to every
client" is not an acceptable outcome of running a sync command.

Working loop: `make:model User -a` → fill in the migration → `cargo luxid migrate`
→ `cargo luxid db:sync` → entity and factory fields refresh; your overrides survive.

> **Entities and factories only.** Form requests carry no `<luxid:fields>`
> markers and `db:sync` does not touch them: what an endpoint accepts is a
> decision, not a reflection of the table. `db:sync` is also a separate command
> rather than something `migrate` runs for you.

### `luxid check`

The safety net for every runtime-typed decision in this design: OpenAPI attributes that no
longer match the body, container bindings that do not exist, `.with()` paths naming no
relation, controllers with no route.

### Compile time as a tracked feature

`luxid new` generates:

```toml
# .cargo/config.toml
[target.x86_64-unknown-linux-gnu]
rustflags = ["-C", "link-arg=-fuse-ld=mold", "-C", "debuginfo=1"]

# Cargo.toml
[profile.dev]              opt-level = 0
[profile.dev.package."*"]  opt-level = 3
```

Architectural half: the app crate stays thin and weight lives in `luxid-*`, so editing a
controller rebuilds only the app crate.

> **Not built.** `serve` starts the server and nothing more. The watching,
> incremental rebuild, restart and printed rebuild time specified here are not
> implemented; `cargo watch -x run` covers the loop in the meantime.

Project goal, benchmarked in CI: **edit-to-restart under 3 seconds on a warm cache.**

---

## 13. Testing

```rust
#[luxid::test(db = crate::support::database)]
async fn it_lists_only_my_users(db: Db) -> Result<()> {
    let me = UserFactory::new().create_one().await?;
    UserFactory::new().count(3)
        .state(move |row| row.team_id = Set(me.team_id))
        .create().await?;
    UserFactory::new().count(2).create().await?;

    app(db).get("/api/v1/users")
       .acting_as(SECRET, me.id)
       .send().await
       .assert_ok()
       .assert_json_count("data", 3);
    Ok(())
}
```

- `#[luxid::test(db = ..)]` wraps each test in a transaction **rolled back at the end** —
  parallel-safe, no truncation, no fixtures. Implemented by handing the app a `Db` backed
  by a single connection holding an open `DatabaseTransaction`. The argument names a
  function returning a `Db`; without it the attribute is `#[tokio::test]` plus `Result`
  unwrapping.
- Factories are generated by `make:model -f` and refreshed by `db:sync`, so they cannot
  drift from the schema. `state` overrides a field; `create_one` ignores `count`.
- `acting_as(secret, subject)` signs a real token, so the request goes *through* the
  guard rather than around it.
- Assertions: `assert_status`, `assert_ok`, `assert_created`, `assert_no_content`,
  `assert_unauthorized`, `assert_forbidden`, `assert_not_found`, `assert_header`,
  `assert_json_path`, `assert_json_count`, `assert_validation_message`,
  `assert_validation_errors(&["email"])`. Every failure prints the response body.

> **Not built.** `assert_no_n_plus_one` was specified here and does not exist.
> Strict relations cover the same ground from the other direction: an endpoint
> that forgets `.with()` fails its test rather than silently issuing a query per
> row.

---

## 14. Scope and roadmap

### 0.1 ships

Routing, groups, resource routes · `HttpContext`, Request/Response · errors + RFC 7807 ·
middleware · container/providers · the data layer, migrations, strict relations · validation
including async `unique`/`exists` · OpenAPI from attributes · auth (JWT + session, policies,
hashing) · CLI with `make:model` · testing (`TestApp`, factories, rollback) · config + env.

### Deferred

| Version | Feature |
|---|---|
| ~~0.2~~ | ~~Inertia.js adapter~~ — built |
| 0.2 | Background jobs, Postgres-backed |
| 0.3 | Events & listeners |
| 0.3 | Mail |
| 0.4 | Cache, storage, scheduler |
| later | Telescope-style dev dashboard |
| later | SSR views |

### Performance

Measured, not asserted. `cargo bench -p luxid --bench overhead` compares five
variants serving byte-identical responses, driven in-process so the numbers
isolate framework cost rather than kernel and TCP behaviour.

| Variant | µs/request | req/s/core | vs salvo |
|---|---:|---:|---:|
| bare salvo | 2.38 | 419,000 | — |
| luxid, no middleware | 3.36 | 298,000 | +0.97 µs |
| luxid + 2 middleware + container | 4.72 | 212,000 | +2.33 µs |
| luxid + JWT guard | 9.34 | 107,000 | +6.95 µs |
| luxid, realistic stack | 12.59 | 79,000 | +10.20 µs |

Reference hardware: Intel i7-4980HQ (2014, 4 cores), single-threaded.

**Read the differences, not the absolutes.** Requests are driven through
`salvo::test::TestClient`, which adds a fixed per-iteration cost to every
variant. The absolute figures are therefore a latency floor, not a throughput
claim for a networked server. Every variant sends an identical request,
including the `authorization` header the unauthenticated ones ignore, so no
driver-side cost is charged to one variant and not another.

What the decomposition says:

* **The framework floor is ~1 µs per request** — context construction, the
  dispatch chain, and response translation. That is the price of `HttpContext`
  and of sealing salvo away.
* **Each middleware layer costs roughly 0.7 µs**, including a container
  resolution. Boxed futures and the owned context, as designed.
* **Authentication dominates the realistic stack.** The JWT guard adds ~4.6 µs,
  of which 3.18 µs is `Jwt::verify` alone with no HTTP involved. That is
  `jsonwebtoken`'s cost, not Luxid's.

#### Measuring on this machine

The reference box runs other workloads (load average ~2.7 during these runs).
Comparisons **within** one `cargo bench` invocation are sound, because every
variant is measured back to back under the same conditions. Comparisons
**across** invocations are not: an early attempt to compare crypto providers in
separate runs produced a confident, reversed conclusion. Differences below
roughly 100 ns are not resolvable here at all.

#### Crypto provider: measured, then declined

`jsonwebtoken` offers `rust_crypto` (pure Rust) and `aws_lc_rs` (C library).
Measured with interleaved runs under matched load:

| Provider | `Jwt::verify` |
|---|---:|
| `rust_crypto` | 3.52 µs / 3.64 µs |
| `aws_lc_rs` | 2.92 µs |

`aws_lc_rs` is roughly **18% faster on verification**, which is about **5% of a
realistic authenticated request**. It requires cmake and a C compiler.

Offering both as features was built and then reverted. Because four crates
depend on `luxid-core`, feature unification silently enables *both* providers
unless every dependency edge sets `default-features = false`, and `jsonwebtoken`
then panics at runtime rather than failing to build. Making that safe means
`cfg`-gating `Jwt` as a public type. That is too much machinery for 5%.

**Decision: `rust_crypto`, hardcoded.** Building a Luxid app requires no C
toolchain. Adding the option later is not a breaking change, and the number
above is what it would buy.

#### Addressed

* The JWT guard no longer copies the bearer token. `bearer_token()` borrows
  `ctx.request` while `ctx.auth` is written through a disjoint field borrow, so
  authenticated routes lose one allocation per request. The saving is below this
  machine's resolution; it is a removal of work, not a measured win.

#### Known and not addressed

* `Container::scope()` allocates per request even when nothing is bound.
  Deliberately left alone: the cost is one small `Arc`, far below what can be
  measured here, and optimising what cannot be measured is how frameworks
  acquire complexity without acquiring speed.
* `Identity` allocates a claims map per request.

### Two roadmap decisions

**Stay on 0.x until jobs land.** API decisions become permanent early in a
public framework; the honest way to keep the surface open is to not claim 1.0 until it has
survived real use.

**Jobs are Postgres-backed, not Redis.** Following Rails 8's `solid_queue`. A Luxid app
needing only a database to run queues, shipped as a single binary, is a materially better
deployment story than anything in the Rust or PHP ecosystems, and keeps `luxid new` → working
app at zero additional infrastructure.

### Inertia is a protocol, not a rewrite

*Built. `luxid-core/src/inertia.rs`, chapter 24 of the tutorial.*

The official Inertia client adapters (React/Vue/Svelte) work against any backend
implementing the protocol, so Luxid implements only the server half: JSON carrying
`component`/`props`/`url`/`version` when `X-Inertia` is present, otherwise an HTML shell
with `data-page`; plus partial reloads, shared props, asset versioning and a 409 on version
mismatch. `Response::redirect` was already 303 and `HttpContext` already reserved the
`inertia` field, so neither needed a breaking change.

**Validation errors decided the design.** Inertia is post-redirect-get: a failed form
redirects back with the errors flashed to the session, not a 422 document. The obvious home
for that — Luxid's error renderer — cannot work, because `write_error` runs after the
`HttpContext`, and therefore the `Session`, has been consumed. It is middleware instead,
keeping a session handle across `next.run` exactly as `SessionGuard` does.

The payoff is that `Error` is untouched. A route without the middleware still answers with
`422 application/problem+json`, so one action, one validator and one `validate()` call serve
both a JSON API and an Inertia frontend; the route group decides the rendering.

Ordering is load-bearing and documented in the scaffold: `Auth::session()` must be outside
`Inertia`, because the session guard writes back with `?` and would never see an `Err`.

**Inertia is opt-in at `luxid new`, not the default.** Most Luxid apps are APIs, and a Node
toolchain is a real cost to impose on someone who did not ask for one.

---

## 15. Risks

| Risk | Mitigation |
|---|---|
| Runtime-typed decisions (container, eager loading, OpenAPI) push errors to runtime | `luxid check` lint, boot-time eager singleton resolution, strict relations on in dev |
| Proc-macro weight degrades compile times | Macros stay thin — code generation, not type-level computation. Rebuild time benchmarked in CI. |
| salvo's smaller ecosystem means writing more integrations | Salvo sealed in one crate; the app-facing API is unaffected if the substrate is ever swapped |
| OpenAPI attributes drift from action bodies | `luxid check` diffs them; CI-enforceable |
| Scope sprawl before 0.1 ships | Roadmap table is the contract; anything not in §14's 0.1 list waits |

## 16. Deferred decisions

Recorded so they are not silently re-litigated. Each has a working default; none block 0.1.

1. **Schema-change migrations have no generator.** `make:model -m` covers table creation
   only. Default: hand-written migration files. Revisit after real use.
2. **Multi-database / read-replica support.** Default: single connection pool in 0.1.
3. **Route model binding** (Laravel's implicit `{user}` → `User`). Default: absent in 0.1;
   `find_or_fail` covers it with one line.
4. **Windows/macOS linker defaults** in the generated `.cargo/config.toml`. Default: mold on
   Linux, platform default elsewhere, resolved at `luxid new` time.

## 17. Implementation status

Recorded because a design document that silently outruns its implementation is
worse than none. Everything below is measured against the code, not intent.

### Built and tested

Routing, groups and typed middleware attachment · `HttpContext` with extensions
· errors and RFC 7807 rendering · the middleware chain · the service container
with boot-time eager resolution and cycle detection · the data layer (models,
typed columns, relations with batched eager loading, scopes, hooks, strict
relations) · migrations · validation with async `unique`/`exists` · JWT
authentication and argon2 hashing · OpenAPI 3.1 · the in-app CLI · `luxid new`
and `make:model` · the test harness with per-test transaction rollback ·
an overhead benchmark.

Configuration — `luxid.toml` layered under the environment, reachable as
`ctx.config` — is built too, and cookie-backed sessions with id rotation on
login, policies, factories and `db:sync`.

Views: the Inertia.js protocol — shell or JSON from one action, partial
reloads, shared props, asset versioning, and validation failures rendered as a
redirect-back with the errors flashed rather than a 422 — plus session flash,
static file serving, and `luxid new` scaffolding a React, Vue or Svelte client.

313 tests, clippy clean, rustfmt clean. A generated application builds
warning-free, migrates, serves, and answers. CI checks all of that, including
that `luxid new` + `make:model -a` still compiles.

### Specified here but not built

| Item | Section |
|---|---|
| `luxid check` | §12 |
| `ctx.db` — the connection is ambient; resolve `Db` from the container | §4 |
| `ctx.events`, `ctx.logger` | §4 |
| `ctx.cache()`, `ctx.mail()`, `ctx.queue()` — the services behind them are 0.3/0.4 | §4, §14 |
| `Request::file`, `files`, `ip`, `all`, `raw` | §4 |
| `Response::stream` | §4 |
| `ctx.auth.user()` — `Auth` carries identity, not the user record | §11 |
| An app-owned exception handler in `src/app.rs`; rendering is not overridable | §5 |
| Nested eager paths — `.with("posts.comments")` is single-level | §9 |
| `db:sync` refreshing form requests — it rewrites entities and factories only | §12 |
| `serve` watching and rebuilding, and printing the rebuild time | §12 |
| `assert_no_n_plus_one` | §13 |
| Inertia error pages — a 404 or 403 on an Inertia route still renders `application/problem+json` rather than an error component | §14 |
| Session writes are lost when an action returns `Err`: `SessionGuard` persists after `next.run(ctx).await?`, and `write_error` can carry neither headers nor a response | §5, §11 |

### Deviations from this document, and why

| Deviation | Reason |
|---|---|
| Relations declared in `#[luxid::model(..)]` args, not `#[has_many] fn posts();` | A bodiless `fn` is not parseable Rust |
| Hooks declared on the derive, not `#[before_save]` in the model block | Hooks must run on the ordinary write path, so the derive must know them |
| OpenAPI built from the route table, not a static registry | A registry means linker tricks, which §2 ruled out |
| `luxid` split into a scaffolding binary and an in-app CLI | Runtime commands need types from the application's crate |
| `jsonwebtoken` provider hardcoded rather than a feature | Feature unification across four crates enables both providers silently |
| Apps depend on `sea-orm`, `sea-orm-migration` and `schemars` directly | Their derives emit crate-qualified paths; a facade re-export cannot satisfy them |
| Synchronous validation rules are Luxid's own, not the `validator` crate | Async rules had to share the same pass and error aggregation |
| Model writes are free functions (`luxid::insert`), not `User::create` / `user.save()` | A write takes a SeaORM `ActiveModel`, which is not the model type |
| Relations are read through generated per-name methods, not `related::<T>()` | Two relations onto the same model would be ambiguous by type |
| Middleware attaches by chained `.middleware(..)` calls, not a tuple | `Middleware` is not implemented for tuples |
| The data-layer trait is `Record`, not `Lucid` | `Lucid` is AdonisJS's ORM name; 0.2.0 renamed it |

Each of these is implemented as described in its own section above; this table
exists so the change is visible rather than buried.
