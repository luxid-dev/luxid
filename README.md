# Luxid

A convention-over-configuration web framework for Rust, shaped by Laravel and —
more directly — by AdonisJS, which solved the same problem this one has: keeping
Laravel's controller ergonomics inside a type system that will not bend.

Built on [salvo](https://salvo.rs), which is sealed inside the framework. Salvo
types never appear in a Luxid signature.

> **Status: 0.2.0, experimental.** The API will change before 1.0 and some
> pieces are missing (see [What is not built](#what-is-not-built)). It builds, it
> is tested, and it runs — but it has not carried a production workload yet.

## Why another one

[loco](https://loco.rs) is Rails-shaped on axum. Luxid is Laravel-shaped on
salvo. Concretely:

- **Controllers take one owned context**, not extractors — so the trait-bound
  error messages that make axum-based frameworks hostile to newcomers do not
  exist here.
- **Validation rules reach the database.** `unique` and `exists` run as
  asynchronous rules in the same pass as the synchronous ones. No Rust framework
  ships these.
- **The error type carries its own HTTP mapping**, so a missing row is a 404
  with no handling in the action body.
- **Compile time is treated as a feature**, with a benchmark rather than a claim.

## Quickstart

```sh
cargo install luxid-cli     # provides the `luxid` binary

luxid new blogapp
cd blogapp

cargo luxid migrate     # SQLite by default; no infrastructure needed
cargo run               # http://127.0.0.1:3000
```

Then scaffold a resource:

```sh
luxid make:model Post -a
```

which writes the model, migration, factory, seeder, policy, form requests, and
an API resource controller — and registers its routes:

```sh
cargo luxid routes
```

```
GET     /api/health      HealthController::show    [1 middleware]
GET     /api/posts       PostsController::index    [1 middleware]
POST    /api/posts       PostsController::store    [1 middleware]
GET     /api/posts/{id}  PostsController::show     [1 middleware]
PUT     /api/posts/{id}  PostsController::update   [1 middleware]
DELETE  /api/posts/{id}  PostsController::destroy  [1 middleware]
```

## A tour

### Controllers

Every action takes an owned `HttpContext` and returns `Result<Response>`.

```rust
#[luxid::controller]
impl PostsController {
    #[openapi(summary = "List posts", tag = "posts")]
    async fn index(ctx: HttpContext) -> Result<Response> {
        let page = ctx.request.input::<u64>("page")?.unwrap_or(1);

        ctx.response.ok(Post::query().published().paginate(page, 20).await?)
    }

    #[openapi(body = StorePost, tag = "posts", errors = [422])]
    async fn store(ctx: HttpContext) -> Result<Response> {
        let input = ctx.request.validate::<StorePost>().await?;

        let post = luxid::insert(posts::ActiveModel {
            title: Set(input.title),
            ..Default::default()
        })
        .await?;

        ctx.response.created(post)
    }

    #[openapi(tag = "posts", errors = [404])]
    async fn show(ctx: HttpContext) -> Result<Response> {
        // A missing row becomes a 404 problem document. No branching here.
        ctx.response.ok(Post::find_or_fail(ctx.params.get::<i64>("id")?).await?)
    }
}
```

Destructuring works too — it is the same type, so it is a style choice:

```rust
async fn store(HttpContext { request, response, .. }: HttpContext) -> Result<Response>
```

### Validation

Synchronous rules run first. Asynchronous rules — the ones that need the
database — run afterwards, skipping fields that already failed, so one mistake
produces one message.

```rust
#[derive(Deserialize, Validate, JsonSchema)]
pub struct StoreUser {
    #[validate(length(min = 2, max = 64))]
    pub name: String,

    #[validate(email, unique(User::email))]      // hits the database
    pub email: String,

    #[validate(exists(Team::id))]                // hits the database
    pub team_id: i64,

    #[validate(range(min = 18, max = 120))]
    pub age: Option<i64>,
}
```

Failures render as RFC 7807:

```json
{ "type": "https://luxid.rs/errors/validation",
  "title": "The given data was invalid",
  "status": 422,
  "errors": { "email": ["has already been taken"] } }
```

### Models

```rust
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, luxid::Model)]
#[sea_orm(table_name = "posts")]
#[luxid(before_create = Self::slugify)]
pub struct Model {
    #[sea_orm(primary_key)] pub id: i64,
    pub title: String,
    pub user_id: i64,

    #[sea_orm(ignore)] #[serde(flatten)] pub relations: luxid::Relations,
}
```

```rust
#[luxid::model(belongs_to(author = User, fk = "user_id"))]
impl Post {
    #[scope]
    fn published(query: Query<posts::Entity>) -> Query<posts::Entity> {
        query.where_eq(Post::published, true)
    }
}
```

Columns are typed, so a mismatched comparison is a compile error:

```rust
Post::query().where_eq(Post::user_id, 7)        // compiles
Post::query().where_eq(Post::user_id, "seven")  // does not
```

Eager loading is batched — one query per relation, whatever the page size — and
loaded relations serialize inline with the model:

```rust
let posts = Post::query().with("author").paginate(1, 20).await?;
posts.data[0].author()?;   // Option<&User>
```

Reading a relation you did not load is an error in development, naming the fix:

```
the `author` relation of `Post` was not loaded. Add `.with("author")` to the query.
```

That turns an N+1 into a failing test.

### Middleware

Same context type as controllers, so there is one mental model.

```rust
#[luxid::middleware]
impl Timer {
    async fn handle(&self, ctx: HttpContext, next: Next) -> Result<Response> {
        let started = Instant::now();
        let response = next.run(ctx).await?;

        Ok(response.header("x-response-time", format!("{}ms", started.elapsed().as_millis())))
    }
}
```

Code above `next.run()` runs on the way in, below on the way out. Attach it
globally, per group, or per route.

### Authorization

A policy is an ordinary function of `(&Auth, &T) -> bool` — no trait, no
registry:

```rust
impl PostPolicy {
    fn update(auth: &Auth, post: &Post) -> bool {
        auth.try_identity().and_then(|i| i.id::<i64>().ok()) == Some(post.user_id)
    }
}
```

```rust
ctx.authorize(PostPolicy::update, &post)?;   // 403 if denied
ctx.can(PostPolicy::update, &post)           // bool, for deciding what to render
```

### Services

ASP.NET Core's lifetime model — the proven answer for a statically typed
language.

```rust
Providers::new()
    .singleton(|_| Settings::from_env())
    .scoped(|_| RequestId::new())
    .bind::<dyn Mailer, _>(|c| Arc::new(Smtp::new(c)))
```

Resolved anywhere the context reaches: `ctx.services.get::<Settings>()?`. Every
singleton is resolved at boot, so a missing binding fails at startup naming the
type, not on first request.

### Sessions

```rust
r.group("/", |r| {
    r.middleware(Auth::session());
    …
});
```

```rust
ctx.session.put("cart", items)?;
ctx.session.get::<Vec<Item>>("cart")?;
ctx.session.login(&Identity::new(user.id.to_string()))?;   // rotates the id
ctx.session.logout()?;
```

The cookie carries an opaque 256-bit id and nothing else; values live in a
`SessionStore`. `login` rotates the id, so an attacker who fixed a victim's
session id before login holds nothing afterwards. `MemoryStore` ships for single
process and tests; the trait is public for anything shared.

Writing to a session on a route without the middleware is an error naming the
fix, not a silent no-op.

### Factories

```rust
UserFactory::new().create_one().await?;
UserFactory::new().count(3).create().await?;
UserFactory::new().state(|row| row.role = Set("admin".into())).create_one().await?;
```

A factory says what a typical row looks like; a test overrides only what it
cares about. `cargo luxid db:sync` reads the live schema and refreshes the
generated field list, touching only what lies between the `<luxid:fields>`
markers — so rules you wrote outside them survive.

### Testing

```rust
#[luxid::test(db = crate::support::database)]
async fn it_lists_posts(db: Db) -> Result<()> {
    app(db).get("/api/posts").send().await
        .assert_ok()
        .assert_json_count("data", 2)
        .assert_json_path("data.0.title", "First");

    Ok(())
}
```

`acting_as` skips the login round-trip while still going through the real guard:

```rust
app(db).get("/me").acting_as(SECRET, user.id).send().await.assert_ok();
```

Each test runs inside a transaction that is rolled back afterwards, so tests
share one database, run in parallel, and need no truncation or fixtures.
Assertion failures print the response body.

## Performance

Measured, not asserted — `cargo bench -p luxid --bench overhead`.

| Variant | µs/request | vs salvo |
|---|---:|---:|
| bare salvo | 2.38 | — |
| luxid, no middleware | 3.36 | +0.97 µs |
| luxid + 2 middleware + container | 4.72 | +2.33 µs |
| luxid, realistic stack | 12.59 | +10.20 µs |

The framework floor is about **1 µs per request**. Authentication dominates a
realistic stack: the JWT guard adds ~4.6 µs, of which 3.18 µs is HS256
verification with no HTTP involved.

Read the differences, not the absolutes — requests are driven in-process, so
these are a latency floor rather than a networked throughput claim. Reference
hardware is a 2014 i7-4980HQ. Method and caveats are in the design document.

## What is built

Routing and groups · `HttpContext` · errors and RFC 7807 · middleware · service
container · models with relations, scopes, hooks and typed columns ·
migrations · validation with async rules · JWT authentication and argon2
hashing · OpenAPI 3.1 · in-app CLI (`serve`, `migrate*`, `routes`, `openapi`) ·
`luxid new` and `make:model` scaffolding · cookie-backed sessions · factories ·
`db:sync` · a test harness with per-test transaction rollback and `acting_as` ·
Inertia.js views with React, Vue or Svelte.

## What is not built

- **Nested eager paths** — `.with("posts.comments")` is single-level only.
- **`luxid check`** — planned, not written.
- **Background jobs** — planned for 0.2.

## A note on dependencies

A generated app declares `sea-orm`, `sea-orm-migration` and `schemars` directly,
alongside `luxid`. Their derive macros emit crate-qualified paths
(`sea_orm::…`, `schemars::…`), so re-exporting the types through `luxid` is not
enough for `#[derive(..)]` to resolve. `luxid new` wires this up for you; it is
worth knowing if you add an entity to a crate that lacks them.

## Development

```sh
cargo test                              # 280 tests
cargo clippy --all-targets
cargo bench -p luxid --bench overhead
```

When working on the framework itself, point a generated app at your checkout:

```sh
luxid new demoapp --luxid-path /path/to/luxid
```

The design document — including the reasoning behind every decision above, the
benchmark methodology, and a status table of what is and is not built — is at
[`docs/design.md`](docs/design.md).

## License

MIT or Apache-2.0, at your option.
