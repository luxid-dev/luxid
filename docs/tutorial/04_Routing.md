# 04 — Routing

A route says: *this method and path go to this action.* Every route in a Luxid
app is registered in one function, so the routing table is something you read
rather than deduce.

## The five verbs

```rust
pub fn register(r: &mut Router) {
    r.get("/posts", PostsController::index);
    r.post("/posts", PostsController::store);
    r.put("/posts/{id}", PostsController::update);
    r.patch("/posts/{id}", PostsController::patch);
    r.delete("/posts/{id}", PostsController::destroy);
}
```

The second argument is an action, referenced **without parentheses**. You are
naming the action, not calling it.

## Route parameters

Curly braces capture a path segment:

```rust
r.get("/posts/{id}", PostsController::show);
```

```rust
async fn show(ctx: HttpContext) -> Result<Response> {
    let id: i64 = ctx.params.get("id")?;
    // ...
}
```

`params.get` decodes into whatever type you ask for. `/posts/abc` requested as
an `i64` produces a `400` with a message naming the parameter — no handling
needed in the action.

Use `try_get` when a parameter is genuinely optional:

```rust
let id: Option<i64> = ctx.params.try_get("id")?;
```

You can capture more than one:

```rust
r.get("/teams/{team}/posts/{id}", PostsController::show);
```

## Groups

A group applies a common prefix:

```rust
r.group("/api/v1", |r| {
    r.get("/posts", PostsController::index);      // /api/v1/posts
    r.get("/posts/{id}", PostsController::show);  // /api/v1/posts/{id}
});
```

Groups nest:

```rust
r.group("/api", |r| {
    r.group("/v1", |r| {
        r.get("/posts", PostsController::index);  // /api/v1/posts
    });

    r.group("/v2", |r| {
        r.get("/posts", v2::PostsController::index);  // /api/v2/posts
    });
});
```

Groups also carry middleware, which is their more important job — see chapter 8.

## Resource routes

Five routes for one resource is a common shape, so there is a shortcut:

```rust
r.resource("/posts", PostsController);
```

That single line registers, for a controller defining all five actions:

| Method | Path | Action |
|---|---|---|
| GET | `/posts` | `index` |
| POST | `/posts` | `store` |
| GET | `/posts/{id}` | `show` |
| PUT | `/posts/{id}` | `update` |
| DELETE | `/posts/{id}` | `destroy` |

Note the argument: `PostsController`, the **struct value**, not an action.

**Only the actions that exist are registered.** A read-only controller:

```rust
#[luxid::controller]
impl ReportsController {
    async fn index(ctx: HttpContext) -> Result<Response> { /* ... */ }
    async fn show(ctx: HttpContext) -> Result<Response> { /* ... */ }
}
```

```rust
r.resource("/reports", ReportsController);
```

registers two routes, not five. You never get a `DELETE` route pointing at an
action that does not exist.

Any *other* action on the controller — say `archive` — is not part of the
resource convention and gets no route. Register it yourself if you want one:

```rust
r.resource("/posts", PostsController);
r.post("/posts/{id}/archive", PostsController::archive);
```

A controller with none of the five resource actions cannot be passed to
`resource` at all — that is a compile error, not a silently empty registration.

## Reading the table

```sh
cargo run -- routes
```

```
GET     /api/posts       PostsController::index    [1 middleware]
POST    /api/posts       PostsController::store    [1 middleware]
GET     /api/posts/{id}  PostsController::show     [1 middleware]
PUT     /api/posts/{id}  PostsController::update   [1 middleware]
DELETE  /api/posts/{id}  PostsController::destroy  [1 middleware]
```

Reach for this whenever an endpoint behaves unexpectedly. It answers:

- Is the route registered at all?
- Is the path what I think it is? (A missing or doubled prefix is common.)
- Is the right action handling it?
- How many middleware wrap it? (A route missing its guard shows up here.)

## Order does not decide matching

Unlike some frameworks, Luxid does not match routes in declaration order — the
underlying router picks the most specific match. So `/posts/{id}` and
`/posts/featured` can coexist, and `featured` will win for that exact path.

One consequence worth knowing: a request to `/posts/archive` where only
`/posts/{id}` is registered *does* match — and then fails when `archive` cannot
be read as an `i64`, producing a `400`. That is the correct behaviour, but it
surprises people expecting a `404`.

## A realistic routing file

```rust
use luxid::prelude::*;

use crate::controllers;

pub fn register(r: &mut Router) {
    r.group("/api", |r| {
        // Public
        r.get("/health", controllers::health_controller::HealthController::show);
        r.post("/register", controllers::auth_controller::AuthController::register);
        r.post("/login", controllers::auth_controller::AuthController::login);

        // Authenticated
        r.group("/", |r| {
            r.middleware(Auth::jwt());

            r.get("/me", controllers::me_controller::MeController::show);
            r.resource("/posts", controllers::posts_controller::PostsController);
        });

        // <luxid:routes>
    });
}
```

Public routes first, then a group carrying the guard. That grouping is the point
of chapter 8.

---

Previous: [03 — Your First App](03_Your_First_App.md) · Next: [05 — Controllers](05_Controllers.md)
