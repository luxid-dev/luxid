# 09 — Services

A service is any object your application wants to share: a database handle, an
HTTP client, a configuration struct, a mailer. The **container** is where you
register them, and `ctx.services` is how actions get them.

## Why not just use globals?

You could put a `static` somewhere and reach for it. The container is better for
three reasons that matter as soon as you write tests:

- You can **swap an implementation** — a fake mailer in tests, a real one in
  production — without changing the code that uses it.
- Objects with **per-request** lifetimes work naturally.
- Missing wiring fails **at startup** with a message naming the type, rather
  than on the first request that needs it.

## Registering

In `src/app.rs`:

```rust
fn providers(db: Db) -> Providers {
    Providers::new()
        .singleton(move |_| db.clone())
        .singleton(|_| Settings { per_page: 20 })
}
```

Each closure receives the container, so services can depend on each other:

```rust
Providers::new()
    .singleton(|_| Settings::from_env())
    .singleton(|c| {
        let settings = c.get::<Settings>().expect("Settings is registered");
        Mailer::new(&settings.smtp_url)
    })
```

## Three lifetimes

```rust
Providers::new()
    .singleton(|_| Settings::from_env())   // once, for the whole app
    .scoped(|_| RequestId::new())          // once per request
    .transient(|_| Formatter::new())       // every time it is resolved
```

**`singleton`** — built once at startup and shared. Use for connection pools,
configuration, clients. Most services are singletons.

**`scoped`** — built once per request, then shared for the rest of it. Use when
a value should be consistent within one request but not across them — a request
id, a per-request cache.

**`transient`** — built fresh every time. Rare; use when the object is stateful
and must not be shared.

## Resolving

From any action or middleware:

```rust
async fn index(ctx: HttpContext) -> Result<Response> {
    let settings = ctx.services.get::<Settings>()?;

    ctx.response.ok(json!({ "per_page": settings.per_page }))
}
```

You get an `Arc<Settings>`. The `?` handles the case where nothing is
registered, producing a redacted `500` with the type name in your logs.

## Swapping implementations

Register a trait rather than a concrete type and you can substitute freely:

```rust
pub trait Mailer: Send + Sync {
    fn send(&self, to: &str, body: &str) -> luxid::Result<()>;
}

pub struct Smtp { /* ... */ }
impl Mailer for Smtp { /* ... */ }

pub struct Collected {
    pub sent: std::sync::Mutex<Vec<String>>,
}
impl Mailer for Collected { /* records instead of sending */ }
```

```rust
// production
Providers::new().bind::<dyn Mailer, _>(|_| Arc::new(Smtp::new()))

// tests
Providers::new().bind::<dyn Mailer, _>(|_| Arc::new(Collected::default()))
```

Resolve a bound trait with `get_dyn` rather than `get`:

```rust
let mailer = ctx.services.get_dyn::<dyn Mailer>()?;
mailer.send(&user.email, "welcome")?;
```

The action is identical in both configurations. That is the whole point.

## Failing at startup, not at 3am

`App::run` resolves **every singleton before binding the port**. A missing
dependency stops the process immediately:

```
error: no provider bound for `app::services::Mailer`.
       Register it in `providers()`, e.g. `.singleton(|_| Mailer::new())`
```

Cyclic dependencies are caught too, and reported as the chain rather than a
stack overflow:

```
error: dependency cycle in providers: Pool → Repo → Pool
```

Tests use `App::into_service()`, which deliberately *skips* this check so a test
can bind only what it needs. `App::try_into_service()` is the validating version
when you want it.

## A worked example

`src/services/mod.rs`:

```rust
pub mod pricing;

// <luxid:modules>
```

`src/services/pricing.rs`:

```rust
use luxid::prelude::*;

pub struct Pricing {
    tax_rate: f64,
}

impl Pricing {
    pub fn new(tax_rate: f64) -> Self {
        Self { tax_rate }
    }

    pub fn with_tax(&self, amount: f64) -> f64 {
        amount * (1.0 + self.tax_rate)
    }
}
```

Register it in `src/app.rs`:

```rust
fn providers(db: Db, config: &Config) -> luxid::Result<Providers> {
    let tax_rate: f64 = config.get_or("pricing.tax_rate", 0.2)?;

    Ok(Providers::new()
        .singleton(move |_| db.clone())
        .singleton(move |_| crate::services::pricing::Pricing::new(tax_rate)))
}
```

Use it:

```rust
async fn quote(ctx: HttpContext) -> Result<Response> {
    let pricing = ctx.services.get::<crate::services::pricing::Pricing>()?;
    let amount = ctx.request.input::<f64>("amount")?.unwrap_or(0.0);

    ctx.response.ok(json!({ "total": pricing.with_tax(amount) }))
}
```

## When not to use a service

Not everything needs registering. A pure function is simpler than a service and
needs no wiring:

```rust
pub fn slugify(title: &str) -> String { /* ... */ }
```

Reach for the container when the thing holds state, owns a connection, or needs
to be swapped in tests. Otherwise, write a function.

---

Previous: [08 — Middleware](08_Middleware.md) · Next: [10 — Configuration](10_Configuration.md)
