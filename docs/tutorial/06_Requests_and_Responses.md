# 06 — Requests and Responses

## Reading input

### `input` — query string or body

The one you will use most:

```rust
let page: Option<u32> = ctx.request.input("page")?;
let page = page.unwrap_or(1);
```

Or in one line:

```rust
let page = ctx.request.input::<u32>("page")?.unwrap_or(1);
```

`input` checks the query string first, then falls back to the JSON body. So
`?page=2` and `{"page": 2}` both work, and your action does not care which the
client used.

Two layers of "might not work" are worth separating:

- **`Option`** — the key was absent. Not an error; you decide the default.
- **`?`** — the key was present but could not be read as the type you asked for.
  That *is* an error, and it becomes a `400` naming the field.

### `query` and `query_all`

When you specifically want the query string:

```rust
let search: Option<String> = ctx.request.query("q")?;
let tags: Vec<String> = ctx.request.query_all("tag")?;   // ?tag=a&tag=b
```

`query` takes the first value of a repeated key; `query_all` takes them all.

### `body_json` — the whole body

```rust
#[derive(Deserialize)]
struct CreatePost {
    title: String,
    body: String,
}

let input: CreatePost = ctx.request.body_json()?;
```

A body that will not deserialize produces a `400`, not a `422`. The distinction
matters: `422` says "these fields are wrong", which implies the client can fix
them one at a time. A body that is not valid JSON at all is a broken request.

For anything user-facing, prefer `validate` over `body_json` — chapter 15.

### Headers and cookies

```rust
let agent = ctx.request.header("user-agent");        // Option<&str>
let token = ctx.request.bearer_token();              // Option<&str>, strips "Bearer "
let session = ctx.request.cookie("luxid_session");   // Option<&str>
```

### Everything else

```rust
ctx.request.method()      // &Method
ctx.request.path()        // &str
ctx.request.uri()         // &Uri
ctx.request.headers()     // &HeaderMap
ctx.request.body_bytes()  // &Bytes — raw, for uploads or signatures
```

## Writing output

`ctx.response` is a builder. Methods come in two kinds.

**Builders** return a `Response` and can be chained:

```rust
ctx.response.status(201).header("x-trace", trace_id)
```

**Terminal methods** return `Result<Response>` and finish the action:

```rust
ctx.response.ok(post)
```

So a typical action ends with exactly one terminal call, optionally after some
builders.

### The terminal methods

```rust
ctx.response.ok(value)         // 200, JSON body
ctx.response.created(value)    // 201, JSON body
ctx.response.accepted(value)   // 202, JSON body
ctx.response.no_content()      // 204, no body
ctx.response.json(value)       // JSON body, whatever status is set
ctx.response.text("hello")     // text/plain
ctx.response.redirect("/here") // 303
ctx.response.bytes(data, "image/png")
```

Anything implementing `serde::Serialize` can be a body — your models, a `Vec`, a
`serde_json::json!` literal, a tuple struct.

### Setting a status yourself

```rust
ctx.response.status(418).json(json!({ "detail": "I'm a teapot" }))
```

An out-of-range status becomes a `500` rather than panicking, on the grounds
that a programming error should not take the process down.

### Headers and cookies

```rust
ctx.response
    .header("x-request-id", id)
    .cookie(Cookie::new("theme", "dark").max_age(86_400))
    .ok(body)
```

Cookies default to `HttpOnly`, `SameSite=Lax`, `Path=/`. Override deliberately:

```rust
Cookie::new("theme", "dark")
    .http_only(false)          // readable from JavaScript
    .secure(true)              // HTTPS only — turn this on in production
    .same_site(SameSite::Strict)
```

## A worked example

```rust
use luxid::prelude::*;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Search {
    term: String,
}

pub struct SearchController;

#[luxid::controller]
impl SearchController {
    async fn index(ctx: HttpContext) -> Result<Response> {
        let page = ctx.request.input::<u32>("page")?.unwrap_or(1);
        let per_page = ctx.request.input::<u32>("per_page")?.unwrap_or(20).min(100);

        ctx.response
            .header("x-page", page.to_string())
            .ok(json!({ "page": page, "per_page": per_page, "results": [] }))
    }

    async fn store(ctx: HttpContext) -> Result<Response> {
        let search: Search = ctx.request.body_json()?;

        if search.term.trim().is_empty() {
            return Err(Error::BadRequest("a search term is required".into()));
        }

        ctx.response.created(json!({ "term": search.term }))
    }
}
```

Two habits worth copying from that: clamping `per_page` so a client cannot ask
for a million rows, and returning early with an explicit error rather than
nesting the happy path inside an `if`.

---

Previous: [05 — Controllers](05_Controllers.md) · Next: [07 — Errors](07_Errors.md)
