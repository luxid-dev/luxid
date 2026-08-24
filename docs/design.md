# Luxid — Design

**Status:** Living document — updated as the implementation proves or
disproves what it says. Started 2026-08-22.
**Substrate:** salvo · SeaORM 2.0 · Rust edition 2024 (toolchain 1.97.1)

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
3. First-class Inertia.js adapter (0.2) — the migration path for Laravel refugees.
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
    pub request:  Request,      // owned
    pub response: Response,     // owned, consumed on terminal methods
    pub params:   Params,
    pub auth:     Auth,
    pub db:       Db,           // Arc handle
    pub services: Container,    // Arc handle
    pub config:   Config,       // Arc handle
    pub events:   Events,       // Arc handle
    pub logger:   Logger,       // Arc handle
}
```

`auth` is present on every route. `auth.user()` returns `Result` (401 if absent);
`auth.try_user()` returns `Option`.

Convenience accessors for core services: `ctx.cache()`, `ctx.mail()`, `ctx.queue()`.

**Request:** `input::<T>` (query + body merged), `query`, `body`, `header`, `cookie`,
`file`, `files`, `ip`, `method`, `url`, `all`, `bearer_token`, `validate::<T>`, `raw()`.

**Response:** builder methods return `Response`; terminal methods return `Result<Response>`.

```rust
response.ok(body)                                   response.no_content()
response.created(body)                              response.redirect(url)
response.status(201).header("x-trace", id).json(v)  response.stream(s)
```

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
    Database(sea_orm::DbErr),       // 500 — logged in full, redacted in the response
    Internal(anyhow::Error),        // 500
    Http { status, code, message, details },
}

pub type Result<T> = std::result::Result<T, Error>;
```

This is what keeps actions short: `User::find_or_fail(id).await?` yields a clean 404 with
no handling in the body.

Rendering goes through an **exception handler generated into `src/app.rs`** and owned by
the app, equivalent to Laravel's `Handler.php`. Default output is RFC 7807 `problem+json`:

```json
{ "type": "https://luxid.rs/errors/validation",
  "title": "The given data was invalid",
  "status": 422,
  "errors": { "email": ["must be a valid email address"] } }
```

RFC 7807 is chosen because it yields free OpenAPI error schemas and free client codegen.

Lifecycle: `middleware → HttpContext built → action → Result<Response> → exception handler (on Err) → salvo response`.

---

## 6. Validation

`derive(Validate)` wraps the `validator` crate for synchronous rules and adds
**asynchronous, DB-backed rules** — the Laravel feature most conspicuously missing from Rust.

```rust
#[derive(Validate, Deserialize)]
pub struct StoreUser {
    #[validate(length(min = 2, max = 64))]     pub name: String,
    #[validate(email, unique(User::email))]    pub email: String,   // async
    #[validate(length(min = 8))]               pub password: String,
    #[validate(exists(Team::id))]              pub team_id: i64,    // async
}
```

`request.validate::<StoreUser>().await?` runs sync rules first, then **all** async rules in
a single batch against `ctx.db`, and aggregates into one 422. Never a per-field round trip,
never a partial report.

`unique` and `exists` correspond to Laravel's `unique:users,email` and `exists:teams,id`.

---

## 7. Middleware

Same `HttpContext` type as controllers — one mental model framework-wide.

```rust
#[luxid::middleware]
impl AuthMiddleware {
    async fn handle(mut ctx: HttpContext, next: Next) -> Result<Response> {
        let token = ctx.request.bearer_token().ok_or(Error::Unauthorized)?;
        ctx.auth.set(Jwt::verify(&token, ctx.config.jwt())?);

        let res = next.run(ctx).await?;
        Ok(res.header("x-authenticated", "1"))
    }
}
```

Code above `next.run()` runs before; below runs after; early return short-circuits. No
separate before/after API.

Attachment is **typed, not string-keyed**, so typos are compile errors rather than runtime
surprises:

```rust
r.resource("/admin", AdminController).middleware((Auth::jwt(), Role::admin()));
```

Order is global → group → route. `luxid routes` prints the resolved stack per route.

---

## 8. Service container

ASP.NET Core's lifetime model — the proven static-language answer.

```rust
// src/app.rs
pub fn providers() -> Providers {
    Providers::new()
        .singleton(|c| UserService::new(c.db()))
        .scoped(|_| RequestId::new())
        .bind::<dyn Mailer>(|c| Arc::new(Smtp::new(c.config().mail())))
}
```

Resolution: `ctx.services.get::<UserService>()?`.

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
execution, and multi-app processes. Context accessors (`ctx.cache()`) read nearly as short
with none of the globals.

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
    .with("posts.comments")
    .order_by_desc(User::created_at)
    .paginate(page, 20).await?     // Paginated<User>

User::create(input).await?;  user.fill(patch).save().await?;  user.delete().await?;
```

### Typed columns

`User::team_id` is a generated zero-sized type carrying `Value = i64`. `.where_eq(User::team_id, "abc")`
fails to compile, where SeaORM's `users::Column::TeamId.eq("abc")` compiles and fails at runtime.

### Eager loading

Runtime-typed, by string path: `.with("posts.comments")`, read back via
`user.related::<Post>()?`. Consistent with the pure-context trade — runtime typing where static
typing would cost more ergonomics than it is worth — and it permits arbitrary-depth graphs,
which a fully static design cannot express in Rust without generic soup.

Loaded relations serialize into the JSON automatically, matching Eloquent.

### Strict relations

`strict_relations = true` in `luxid.toml`, default **on in dev, off in prod**. Accessing an
unloaded relation raises an error naming the missing `.with()` path. Turns N+1 from a
production surprise into a failing test.

### Transactions

Explicit: `ctx.db.transaction(|tx| async move { ... }).await?`.

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

ctx.auth.user().await?
ctx.auth.try_user().await?
ctx.authorize(PostPolicy::update, &post)?   // → Error::Forbidden → 403
ctx.can(PostPolicy::update, &post)          // bool, no consequence
```

`Hash` wraps argon2; tokens use `jsonwebtoken`. `Guard` is a public trait, so API-key and
OAuth guards need no Luxid release.

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
  migrate:fresh | migrate:status | routes | openapi`, wired up by one line in
  `main.rs`:

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
empty. `luxid db:sync` (run automatically after `luxid migrate`) refreshes the mechanical
portion inside marked regions while preserving hand-written rules:

```rust
pub struct StoreUser {
    // <luxid:fields>  regenerated by `luxid db:sync`
    #[validate(length(max = 255))]           pub name: String,
    #[validate(email, unique(User::email))]  pub email: String,
    // </luxid:fields>

    #[validate(length(min = 8))]             pub password: String,   // preserved
}
```

Working loop: `make:model User -a` → fill in the migration → `luxid migrate` → entities,
factory fields, and form-request fields refresh; your overrides survive.

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
controller rebuilds only the app crate. `luxid serve` watches, rebuilds incrementally,
restarts, and **prints the rebuild time** so the number stays visible.

Project goal, benchmarked in CI: **edit-to-restart under 3 seconds on a warm cache.**

---

## 13. Testing

```rust
#[luxid::test]
async fn it_lists_only_my_users(app: TestApp) -> Result<()> {
    let me = UserFactory::new().create().await?;
    UserFactory::new().count(3).for_team(me.team_id).create().await?;
    UserFactory::new().count(2).create().await?;

    app.get("/api/v1/users").acting_as(&me).send().await?
       .assert_ok()
       .assert_json_count("data", 3);
    Ok(())
}
```

- `#[luxid::test]` wraps each test in a transaction **rolled back at the end** — parallel-safe,
  no truncation, no fixtures. Implemented by handing the app a `Db` backed by a single
  connection holding an open `DatabaseTransaction`.
- Factories are generated by `make:model -f` and refreshed by `db:sync`, so they cannot
  drift from the schema.
- `acting_as` skips the login round-trip.
- Assertions: `assert_status`, `assert_ok`, `assert_json_path`, `assert_json_count`,
  `assert_validation_errors(&["email"])`, `assert_no_n_plus_one`.

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
| 0.2 | Inertia.js adapter |
| 0.2 | Background jobs, Postgres-backed |
| 0.3 | Events & listeners |
| 0.3 | Mail |
| 0.4 | Cache, storage, scheduler |
| later | Telescope-style dev dashboard |
| later | SSR views |

### Performance

Measured, not asserted. `cargo bench -p luxid --bench overhead` compares three
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

**Stay on 0.x until Inertia and jobs land.** API decisions become permanent early in a
public framework; the honest way to keep the surface open is to not claim 1.0 until it has
survived real use.

**Jobs are Postgres-backed, not Redis.** Following Rails 8's `solid_queue`. A Luxid app
needing only a database to run queues, shipped as a single binary, is a materially better
deployment story than anything in the Rust or PHP ecosystems, and keeps `luxid new` → working
app at zero additional infrastructure.

### Inertia is a protocol, not a rewrite

The official Inertia client adapters (React/Vue/Svelte) work against any backend
implementing the protocol. Luxid needs only the server half: respond with JSON carrying
`component`/`props`/`url`/`version` when `X-Inertia` is present, otherwise render an HTML
shell with `data-page`; plus partial reloads via `X-Inertia-Partial-Data`, shared props,
asset versioning, and 303 redirects on PUT/PATCH/DELETE.

The 0.1 response abstraction must therefore be able to resolve into either JSON or an HTML
shell from the same action. `#[non_exhaustive]` on `HttpContext` reserves room for the
`inertia` field.

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

275 tests, clippy clean, rustfmt clean. A generated application builds
warning-free, migrates, serves, and answers. CI checks all of that, including
that `luxid new` + `make:model -a` still compiles.

### Specified here but not built

| Item | Section |
|---|---|
| `luxid check` | §12 |
| `ctx.auth.user()` — `Auth` carries identity, not the user record | §11 |
| Nested eager paths — `.with("posts.comments")` is single-level | §9 |

### Deviations from this document, and why

| Deviation | Reason |
|---|---|
| Relations declared in `#[luxid::model(..)]` args, not `#[has_many] fn posts();` | A bodiless `fn` is not parseable Rust |
| Hooks declared on the derive, not `#[before_save]` in the model block | Hooks must run on the ordinary write path, so the derive must know them |
| OpenAPI built from the route table, not a static registry | A registry means linker tricks, which §2 ruled out |
| `luxid` split into a scaffolding binary and an in-app CLI | Runtime commands need types from the application's crate |
| `jsonwebtoken` provider hardcoded rather than a feature | Feature unification across four crates enables both providers silently |
| Apps depend on `sea-orm`, `sea-orm-migration` and `schemars` directly | Their derives emit crate-qualified paths; a facade re-export cannot satisfy them |

Each of these is implemented as described in its own section above; this table
exists so the change is visible rather than buried.
