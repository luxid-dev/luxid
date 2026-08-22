# 07 — Errors

This chapter explains why Luxid controllers have almost no error handling in
them, and what your clients see when something goes wrong.

## One error type

Every action returns `Result<Response>`, where the error is `luxid::Error`. Each
variant already knows its HTTP status:

| Variant | Status | Use it when |
|---|---|---|
| `Error::Validation(errors)` | 422 | Input failed its rules |
| `Error::NotFound { .. }` | 404 | The thing does not exist |
| `Error::Unauthorized` | 401 | Not signed in |
| `Error::Forbidden` | 403 | Signed in, but not allowed |
| `Error::Conflict(msg)` | 409 | Clashes with existing state |
| `Error::TooManyRequests` | 429 | Rate limited |
| `Error::BadRequest(msg)` | 400 | Malformed request |
| `Error::Internal(err)` | 500 | Something broke |
| `Error::Http { .. }` | you choose | Anything else |

## Why `?` is enough

Because each variant carries its status, `?` turns a failure into a correct HTTP
response with no handling at the call site:

```rust
async fn show(ctx: HttpContext) -> Result<Response> {
    let post = Post::find_or_fail(ctx.params.get::<i64>("id")?).await?;
    ctx.response.ok(post)
}
```

Two things there can fail, and both are handled:

- `params.get` on a non-numeric id → `400`
- `find_or_fail` on a missing row → `404`

Compare with what you would otherwise write:

```rust
// The same endpoint, without the error type doing any work
async fn show(ctx: HttpContext) -> Result<Response> {
    let raw = ctx.params.raw("id").ok_or_else(|| /* 400 */)?;
    let id: i64 = raw.parse().map_err(|_| /* 400 */)?;

    match Post::find(id).await {
        Ok(Some(post)) => ctx.response.ok(post),
        Ok(None) => Err(/* 404 */),
        Err(e) => Err(/* 500 */),
    }
}
```

Same behaviour, five times the code, and three chances to get a status wrong.

## What the client sees

Errors render as [RFC 7807](https://www.rfc-editor.org/rfc/rfc7807) problem
documents — a small standard for API errors, so clients and code generators
already know the shape.

```json
{
  "type": "https://luxid.rs/errors/not-found",
  "title": "Post `42` not found",
  "status": 404,
  "resource": "Post",
  "id": "42"
}
```

Validation failures add an `errors` object keyed by field:

```json
{
  "type": "https://luxid.rs/errors/validation",
  "title": "The given data was invalid",
  "status": 422,
  "errors": {
    "email": ["must be a valid email address"],
    "name": ["must be at least 2 characters"]
  }
}
```

The `Content-Type` is `application/problem+json`, not `application/json`, which
lets a client tell an error apart from a successful body without reading it.

## Internal errors are redacted

`Error::Internal` is the one variant whose message never reaches the client:

```rust
Err(Error::internal(format!("could not reach {}", connection_string)))
```

The client gets:

```json
{ "type": "https://luxid.rs/errors/internal", "title": "internal server error", "status": 500 }
```

while the full message — connection string and all — goes to your logs. This is
deliberate: internal errors routinely contain hostnames, credentials, and query
fragments, and a framework that leaks them by default is a framework that leaks
them in production.

Everything else uses the message you gave it, so put client-facing wording in
the other variants and diagnostic detail in `Internal`.

## Raising errors

```rust
// Simple cases
return Err(Error::Unauthorized);
return Err(Error::Forbidden);
return Err(Error::Conflict("that email is already registered".into()));

// A 404 that names what was missing
return Err(Error::not_found("Post", id));

// A 500 with a diagnostic message, without needing anyhow in scope
return Err(Error::internal("the payment gateway returned nothing"));

// Validation, built by hand
let mut errors = ValidationErrors::new();
errors.add("title", "is required");
return Err(Error::Validation(errors));

// Anything else
return Err(Error::Http {
    status: 402,
    code: "payment-required".into(),
    message: "your subscription has lapsed".into(),
    details: None,
});
```

## Converting other errors

`?` works on any error type with a `From` conversion into `luxid::Error`.
`serde_json::Error` already converts to a `400`. For your own types:

```rust
impl From<PaymentError> for Error {
    fn from(err: PaymentError) -> Self {
        match err {
            PaymentError::CardDeclined => Error::Conflict("card declined".into()),
            PaymentError::Network(e) => Error::internal(format!("gateway: {e}")),
        }
    }
}
```

Now `charge_card().await?` inside an action produces the right status
automatically. This is where to encode "which of my failures is the client's
fault" — once, rather than at every call site.

## Choosing the right one

A rule that resolves most cases:

- Can the client fix it by changing a field? → `Validation` (422)
- Can they fix it by changing the request some other way? → `BadRequest` (400)
- Do they need to sign in? → `Unauthorized` (401)
- Are they signed in but not permitted? → `Forbidden` (403)
- Does the thing simply not exist? → `NotFound` (404)
- Is it your fault? → `Internal` (500)

The 401/403 distinction is worth getting right: `401` means "I do not know who
you are", `403` means "I know, and no".

---

Previous: [06 — Requests and Responses](06_Requests_and_Responses.md) · Next: [08 — Middleware](08_Middleware.md)
