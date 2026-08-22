# 05 — Controllers

A controller groups related actions. An action handles one endpoint.

## The shape

```rust
use luxid::prelude::*;
use serde_json::json;

pub struct PostsController;

#[luxid::controller]
impl PostsController {
    async fn index(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({ "posts": [] }))
    }

    async fn show(ctx: HttpContext) -> Result<Response> {
        let id: i64 = ctx.params.get("id")?;
        ctx.response.ok(json!({ "id": id }))
    }
}
```

Every action has the same signature:

```rust
async fn name(ctx: HttpContext) -> Result<Response>
```

`async` because almost everything real is asynchronous. One `HttpContext` in, a
`Result<Response>` out. That is the whole contract.

## What `#[luxid::controller]` does

For each action in the block, it generates a route handler and exposes it under
the action's name. That is why `r.get("/posts", PostsController::index)` works —
`PostsController::index` is something the macro created.

It leaves everything else alone. Helper functions, associated constants, and
methods taking `&self` are untouched:

```rust
#[luxid::controller]
impl PostsController {
    async fn index(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({ "per_page": Self::per_page() }))
    }

    // Not an action: takes no context. Left exactly as written.
    fn per_page() -> u32 {
        20
    }
}
```

The rule is mechanical: an `async fn` taking exactly one argument that is not
`self` becomes an action. Everything else does not.

## What is in the context

`HttpContext` carries eight things:

| Field | What it is | Chapter |
|---|---|---|
| `ctx.request` | The incoming request | 06 |
| `ctx.response` | A response builder | 06 |
| `ctx.params` | Route parameters | 04 |
| `ctx.auth` | Who the request is | 16 |
| `ctx.session` | Cookie-backed state | 17 |
| `ctx.services` | Your registered services | 09 |
| `ctx.config` | Configuration | 10 |
| `ctx.extensions` | A typed bag middleware can write to | 08 |

There is deliberately no `ctx.db`. Queries do not need one — the database is
*ambient* within a request, so `Post::find(id).await?` just works. On the rare
occasion you need the handle itself (to open a transaction), resolve it like any
other service: `ctx.services.get::<Db>()?`.

An action uses two or three of these. They are all there so you never have to
change a signature to reach one.

## The two styles

Because `HttpContext` is an ordinary struct, you can destructure it:

```rust
async fn store(HttpContext { request, response, .. }: HttpContext) -> Result<Response> {
    let input: Value = request.body_json()?;
    response.created(input)
}
```

That is the same type — it is a style choice, not a different mode. The `..` is
required and is deliberately so: it means new fields can be added to
`HttpContext` in future versions without breaking your code.

Most people find the short signature easier to read, and destructure inside the
body when they want short names:

```rust
async fn store(ctx: HttpContext) -> Result<Response> {
    let HttpContext { request, response, .. } = ctx;
    // ...
}
```

Use whichever you prefer; the tutorial uses `ctx: HttpContext` throughout.

## One thing that catches everyone

This does not compile:

```rust
ctx.response.ok(json!({ "id": ctx.params.get::<i64>("id")? }))   // ✗
```

`ctx.response.ok(...)` **moves** the response out of `ctx` before the argument is
evaluated, so the argument cannot also use `ctx`. Bind first:

```rust
let id: i64 = ctx.params.get("id")?;                            // ✓
ctx.response.ok(json!({ "id": id }))
```

This is ordinary Rust move semantics rather than anything Luxid invented, but it
is the error new users hit most often.

## Organising controllers

One controller per resource, named plurally, in a file named after it:

```
src/controllers/
├── mod.rs
├── auth_controller.rs        AuthController
├── posts_controller.rs       PostsController
└── comments_controller.rs    CommentsController
```

`luxid make:model Post -c` produces exactly this and registers the routes. You
can of course write them by hand.

## Keeping actions short

An action should read like a summary of what the endpoint does. When it grows
past a screen, the usual culprits and their homes:

| The action is doing... | Move it to | Chapter |
|---|---|---|
| checking input | a validator | 15 |
| deciding permission | a policy | 18 |
| reusable query filtering | a scope | 14 |
| something on every request | middleware | 08 |
| business logic used in several places | a service | 09 |

A well-factored action is often four lines:

```rust
async fn store(ctx: HttpContext) -> Result<Response> {
    let input = ctx.request.validate::<StorePost>().await?;
    let post = luxid::insert(posts::ActiveModel { /* ... */ }).await?;

    ctx.response.created(post)
}
```

---

Previous: [04 — Routing](04_Routing.md) · Next: [06 — Requests and Responses](06_Requests_and_Responses.md)
