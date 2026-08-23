# 08 — Middleware

Middleware is code that runs *around* a request: before the action, after it, or
both. Logging, authentication, rate limiting, and timing are all middleware.

## Writing one

```rust
use luxid::prelude::*;
use std::time::Instant;

pub struct Timer;

#[luxid::middleware]
impl Timer {
    async fn handle(&self, ctx: HttpContext, next: Next) -> Result<Response> {
        let started = Instant::now();

        let response = next.run(ctx).await?;

        Ok(response.header("x-response-time", format!("{}ms", started.elapsed().as_millis())))
    }
}
```

The shape is always the same:

```rust
async fn handle(&self, ctx: HttpContext, next: Next) -> Result<Response>
```

`next` is the rest of the chain — the remaining middleware and, at the end, the
action. Calling `next.run(ctx)` continues; not calling it stops.

Note the same `HttpContext` type as controllers. There is one mental model for
the whole framework.

## Before, after, and instead

There is no separate "before" and "after" API, because ordinary code position is
enough:

```rust
async fn handle(&self, ctx: HttpContext, next: Next) -> Result<Response> {
    // BEFORE — runs on the way in

    let response = next.run(ctx).await?;

    // AFTER — runs on the way out

    Ok(response)
}
```

To reject a request outright, return without calling `next`:

```rust
pub struct BlockRobots;

#[luxid::middleware]
impl BlockRobots {
    async fn handle(&self, ctx: HttpContext, next: Next) -> Result<Response> {
        if ctx.request.header("user-agent").is_some_and(|ua| ua.contains("bot")) {
            return Err(Error::Forbidden);
        }

        next.run(ctx).await
    }
}
```

The action never runs.

## Attaching it

Three levels, from widest to narrowest.

**Global** — every route in the application:

```rust
App::new()
    .middleware(Timer)
    .routes(routes::register)
```

**Group** — every route inside it:

```rust
r.group("/admin", |r| {
    r.middleware(Auth::jwt());

    r.get("/stats", AdminController::stats);
    r.get("/users", AdminController::users);
});
```

**Route** — one endpoint:

```rust
r.get("/me", MeController::show).middleware(Auth::jwt());
```

Or across a whole resource:

```rust
r.resource("/posts", PostsController).middleware(Auth::jwt());
```

Several at once:

```rust
r.get("/admin", AdminController::show).middleware((Auth::jwt(), Role::admin()));
```

Middleware is attached by **value**, not by a string name, so a typo is a
compile error rather than a route that silently runs unguarded.

## Order

Middleware runs outermost first: global, then group, then route. On the way out
it unwinds in reverse.

With `Timer` global and `Auth::jwt()` on a group:

```
→ Timer starts
  → Auth checks the token
    → the action runs
  ← Auth returns
← Timer adds its header
```

You can see the depth per route:

```sh
cargo luxid routes
```

```
GET  /api/health  HealthController::show  [1 middleware]
GET  /api/me      MeController::show      [2 middleware]
```

If a route that should be guarded shows a lower number than its neighbours,
that is your bug.

## Passing data to the action

Middleware often computes something the action needs. `ctx.extensions` is a
typed bag for exactly that:

```rust
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

pub struct AssignRequestId;

#[luxid::middleware]
impl AssignRequestId {
    async fn handle(&self, mut ctx: HttpContext, next: Next) -> Result<Response> {
        let id = RequestId(luxid::session::new_id());   // a 256-bit random id
        ctx.extensions.insert(id.clone());

        Ok(next.run(ctx).await?.header("x-request-id", id.0))
    }
}
```

The action reads it back by type:

```rust
async fn show(ctx: HttpContext) -> Result<Response> {
    let id = ctx.extensions.get::<RequestId>().map(|r| r.0.clone());
    ctx.response.ok(json!({ "request_id": id }))
}
```

Note `mut ctx` in the middleware — writing to the context needs it.

## Errors skip the after-part

If anything downstream fails, the `?` propagates and the code after `next.run`
does not execute:

```rust
let response = next.run(ctx).await?;   // ← an error returns here
Ok(response.header("x-trace", "1"))    // ← never reached
```

That is usually what you want. When you need cleanup regardless of outcome,
match rather than use `?`:

```rust
let outcome = next.run(ctx).await;

// runs either way
metrics.record(started.elapsed());

outcome
```

## Middleware with configuration

Because `handle` takes `&self`, middleware can hold state:

```rust
pub struct RequireHeader {
    name: &'static str,
}

impl RequireHeader {
    pub fn new(name: &'static str) -> Self {
        Self { name }
    }
}

#[luxid::middleware]
impl RequireHeader {
    async fn handle(&self, ctx: HttpContext, next: Next) -> Result<Response> {
        if ctx.request.header(self.name).is_none() {
            return Err(Error::BadRequest(format!("the `{}` header is required", self.name)));
        }

        next.run(ctx).await
    }
}
```

```rust
r.post("/webhook", WebhookController::receive)
    .middleware(RequireHeader::new("x-signature"));
```

This is how the built-in guards work: `Auth::jwt()` returns a configured value.

## Built-in middleware

| | What it does | Chapter |
|---|---|---|
| `WithDatabase` | Makes the database available. Every app needs it. | 11 |
| `WithRollbackDatabase` | As above, but rolls back after each request. Tests only. | 20 |
| `Auth::jwt()` | Requires a valid bearer token | 16 |
| `Auth::optional_jwt()` | Reads a token if present, allows anonymous | 16 |
| `Auth::session()` | Cookie-backed sessions | 17 |

---

Previous: [07 — Errors](07_Errors.md) · Next: [09 — Services](09_Services.md)
