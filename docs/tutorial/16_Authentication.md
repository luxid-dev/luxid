# 16 — Authentication

Authentication answers *who is this?* Authorization — chapter 18 — answers *may
they do this?* Keep them separate in your head; they are separate in the code.

Luxid offers two mechanisms:

- **JWT tokens** — for APIs, mobile clients, anything that can hold a token.
  This chapter.
- **Sessions** — for browsers. Chapter 17.

## Passwords

Never store a password. Store a hash.

```rust
use luxid::prelude::*;

let hash = Hash::make("correct horse battery staple")?;   // store this
let ok = Hash::verify("correct horse battery staple", &hash);   // bool
```

`Hash::make` uses argon2id with a fresh random salt, so the same password hashes
differently every time — which is the point. `Hash::verify` handles the salt for
you.

Two behaviours worth knowing:

- A **corrupt stored hash** fails verification rather than erroring, so a mangled
  database row is indistinguishable from a wrong password.
- Hashing is **deliberately slow**. That is what makes stolen hashes expensive to
  crack, and it is why you hash on registration and login rather than on every
  request.

The reliable way to never store plaintext is a hook (chapter 14), so no code path
can bypass it.

## Tokens

A JSON Web Token says "the bearer is subject X" and is signed so it cannot be
forged.

```rust
let jwt = Jwt::new(secret);

let identity = Identity::new("42").with_claim("role", "admin");
let token = jwt.sign(&identity)?;

let identity = jwt.verify(&token)?;
identity.subject();                       // "42"
identity.id::<i64>()?;                    // 42
identity.claim::<String>("role")?;        // Some("admin")
```

A **subject** is who the token is for — usually a user id as a string. **Claims**
are extra facts you attach.

Configure the signer once:

```rust
Providers::new()
    .singleton(move |_| Jwt::new(&secret).with_ttl(Duration::from_secs(3600)))
```

The default lifetime is fourteen days.

> A token is **signed, not encrypted**. Anyone holding one can read its claims.
> Put identifiers and roles in there; never put anything secret.

## Guarding routes

```rust
r.group("/api", |r| {
    r.post("/login", AuthController::login);          // public

    r.group("/", |r| {
        r.middleware(Auth::jwt());                     // everything below needs a token

        r.get("/me", MeController::show);
        r.resource("/posts", PostsController);
    });
});
```

`Auth::jwt()` reads the `Authorization: Bearer …` header, verifies the token, and
puts the identity on the context. No token, or a bad one, and the action never
runs — the client gets a `401`.

For endpoints that render differently when signed in but allow anonymous access:

```rust
r.get("/feed", FeedController::index).middleware(Auth::optional_jwt());
```

## Reading the user

```rust
async fn show(ctx: HttpContext) -> Result<Response> {
    let id: i64 = ctx.auth.id()?;                          // 401 if anonymous
    let role: Option<String> = ctx.auth.identity()?.claim("role")?;

    ctx.response.ok(json!({ "id": id, "role": role }))
}
```

| Method | Returns |
|---|---|
| `ctx.auth.check()` | `bool` — is anyone signed in? |
| `ctx.auth.id::<T>()` | the subject, parsed. `401` if anonymous |
| `ctx.auth.identity()` | `&Identity`. `401` if anonymous |
| `ctx.auth.try_identity()` | `Option<&Identity>` — never fails |

Use `try_identity` behind `optional_jwt`, and `id`/`identity` behind `jwt`.

`ctx.auth` carries the *identity*, not the user row. To load the row:

```rust
let user = User::find_or_fail(ctx.auth.id::<i64>()?).await?;
```

## A login endpoint

```rust
use luxid::prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::models::user::User;

#[derive(Deserialize, Validate)]
pub struct Credentials {
    #[validate(email)]
    pub email: String,
    #[validate(length(min = 1))]
    pub password: String,
}

pub struct AuthController;

#[luxid::controller]
impl AuthController {
    async fn login(ctx: HttpContext) -> Result<Response> {
        let input = ctx.request.validate::<Credentials>().await?;

        let user = User::query()
            .where_eq(User::email, input.email)
            .first()
            .await?;

        // One branch for both failures: a wrong email and a wrong password must
        // be indistinguishable, or the endpoint tells attackers which addresses
        // are registered.
        let Some(user) = user.filter(|u| Hash::verify(&input.password, &u.password)) else {
            return Err(Error::Unauthorized);
        };

        let jwt = ctx.services.get::<Jwt>()?;
        let identity = Identity::new(user.id.to_string());

        ctx.response.ok(json!({ "token": jwt.sign(&identity)? }))
    }
}
```

That comment is the important part of the endpoint. It is easy to write

```rust
let user = User::query()... .first().await?.ok_or(Error::not_found("User", email))?;
if !Hash::verify(...) { return Err(Error::Unauthorized); }
```

and thereby tell anyone who asks which email addresses have accounts.

## Verification failures do not explain themselves

Expired, forged, and malformed tokens all produce a byte-identical `401`:

```json
{ "type": "https://luxid.rs/errors/unauthorized", "title": "unauthenticated", "status": 401 }
```

Deliberately — a caller who can tell "expired" from "bad signature" can probe
your signing key. If you need to distinguish them, do it in your logs.

## Choosing a secret

```sh
# .env, never committed
APP_KEY=$(openssl rand -hex 32)
```

```rust
let secret: String = config.get("app.key")?;
```

Changing it invalidates every issued token, which is your emergency
"log everyone out" switch.

## Adding a guard of your own

`Auth::jwt()` is ordinary middleware, so an API-key or OAuth guard needs no
framework release:

```rust
pub struct ApiKey;

#[luxid::middleware]
impl ApiKey {
    async fn handle(&self, mut ctx: HttpContext, next: Next) -> Result<Response> {
        let presented = ctx.request.header("x-api-key").ok_or(Error::Unauthorized)?;
        let expected: String = ctx.config.get("app.api_key")?;

        if presented != expected {
            return Err(Error::Unauthorized);
        }

        ctx.auth.set(Identity::new("service"));
        next.run(ctx).await
    }
}
```

Downstream actions read `ctx.auth` exactly as they would behind the JWT guard.

---

Previous: [15 — Validation](15_Validation.md) · Next: [17 — Sessions](17_Sessions.md)
