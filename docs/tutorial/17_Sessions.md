# 17 — Sessions

Tokens suit clients that can store one. Browsers are better served by a cookie,
and that is what sessions are for.

## How it works

The browser holds a cookie containing an **opaque id and nothing else**. The
values live server-side in a store. So the client cannot read what is in the
session, and cannot forge it.

```
Browser                        Server
   │ ── request + cookie ────────▶ │
   │                               │ look up the id in the store
   │                               │ run the action with that session
   │ ◀───── response + cookie ──── │ save any changes
```

## Setting it up

A store, registered like any service:

```rust
use std::sync::Arc;

Providers::new()
    .bind::<dyn SessionStore, _>(|_| Arc::new(MemoryStore::new()))
```

Then the middleware:

```rust
r.group("/", |r| {
    r.middleware(Auth::session());

    r.get("/cart", CartController::show);
    r.post("/login", AuthController::login);
});
```

Note it goes on **public routes too**, including login — a session is how a user
*becomes* authenticated, so anonymous requests pass through rather than being
rejected.

`MemoryStore` keeps sessions in the process. Sessions are lost on restart and are
not shared between instances, so it suits a single process and tests. The
`SessionStore` trait is public for anything shared.

## Reading and writing

```rust
async fn show(ctx: HttpContext) -> Result<Response> {
    let visits: u32 = ctx.session.get("visits")?.unwrap_or(0);
    ctx.session.put("visits", visits + 1)?;

    ctx.response.ok(json!({ "visits": visits }))
}
```

| Method | |
|---|---|
| `get::<T>(key)` | `Option<T>` |
| `put(key, value)` | store anything `Serialize` |
| `has(key)` | `bool` |
| `forget(key)` | remove one value |
| `flush()` | remove all values, keep the session |
| `destroy()` | invalidate entirely |
| `id()` | the session id |

Notice `put` takes `&self`, not `&mut self` — the session is a shared handle, so
you do not need `mut ctx`.

## Logging in and out

```rust
async fn login(ctx: HttpContext) -> Result<Response> {
    let input = ctx.request.validate::<Credentials>().await?;
    let user = /* look up and verify, as in chapter 16 */;

    ctx.session.login(&Identity::new(user.id.to_string()))?;

    ctx.response.ok(json!({ "ok": true }))
}

async fn logout(ctx: HttpContext) -> Result<Response> {
    ctx.session.logout()?;
    ctx.response.no_content()
}
```

On subsequent requests, `Auth::session()` reads the session and populates
`ctx.auth` — so `ctx.auth.id::<i64>()?` works exactly as it does behind the JWT
guard. Your actions do not care which mechanism signed the user in.

## Why `login` rotates the id

`session.login()` does two things: it assigns a **new** session id, then records
the subject.

The rotation is not incidental. Without it, an attacker who plants a known
session id in a victim's browser before they log in still holds a valid id
*afterwards* — a **session fixation** attack, and a complete account takeover.

Rotate whenever privilege changes. `login()` does it for you; `regenerate()` is
there if you change privileges some other way.

`logout()` destroys the store entry *and* clears the cookie, so the old value is
worthless even if it was captured.

## Cookie settings

Defaults are the safe ones: `HttpOnly` (not readable from JavaScript),
`SameSite=Lax`, `Path=/`, and a fourteen-day lifetime.

```rust
r.middleware(
    Auth::session()
        .secure(true)                          // HTTPS only — turn on in production
        .ttl(Duration::from_secs(60 * 60))     // one hour
        .cookie("my_app_session"),
);
```

Turn on `secure` in production. Without it the cookie travels over plain HTTP
where anyone on the network can take it.

## Failure modes

**An unknown or expired cookie starts a fresh session** rather than failing. A
stale cookie is ordinary — a restarted store, an expired entry — not an error.

**Writing without the middleware is an error**, not a silent no-op:

```
no session is active on this route. Add `.middleware(Auth::session())`,
and bind a `SessionStore` in `providers()`.
```

A session write that vanished silently would be an extremely annoying bug to
find.

## Sessions or tokens?

| | Sessions | Tokens |
|---|---|---|
| Client | browsers | anything |
| Carried in | a cookie | a header |
| State | server-side | in the token |
| Revoking | delete the entry | wait for expiry, or keep a list |
| Scaling | needs a shared store | stateless |
| CSRF | needs consideration | not applicable |

Building a browser app? Sessions. A mobile or third-party API? Tokens. Both?
Register both guards and put them on different route groups — `ctx.auth` reads
the same either way.

---

Previous: [16 — Authentication](16_Authentication.md) · Next: [18 — Authorization](18_Authorization.md)
