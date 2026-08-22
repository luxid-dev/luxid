# 18 — Authorization

Authentication established *who*. Authorization decides *whether they may*.

## A policy is a function

No trait to implement, no registry to populate:

```rust
// src/policies/post_policy.rs
use luxid::prelude::*;

use crate::models::post::Post;

pub struct PostPolicy;

impl PostPolicy {
    pub fn view(_auth: &Auth, _post: &Post) -> bool {
        true
    }

    pub fn update(auth: &Auth, post: &Post) -> bool {
        auth.try_identity()
            .and_then(|identity| identity.id::<i64>().ok())
            .is_some_and(|id| id == post.user_id)
    }

    pub fn delete(auth: &Auth, post: &Post) -> bool {
        Self::update(auth, post)
    }
}
```

The signature is always `(&Auth, &T) -> bool`. Anything matching it is a policy.

## Enforcing it

```rust
async fn update(ctx: HttpContext) -> Result<Response> {
    let post = Post::find_or_fail(ctx.params.get::<i64>("id")?).await?;

    ctx.authorize(PostPolicy::update, &post)?;

    // Past this line, they are allowed.
}
```

Denied means `403`, through the ordinary error path. One line, no branching.

Note the policy is passed **without parentheses** — you are naming the function,
not calling it.

## Asking without enforcing

```rust
async fn show(ctx: HttpContext) -> Result<Response> {
    let post = Post::find_or_fail(ctx.params.get::<i64>("id")?).await?;

    let can_edit = ctx.can(PostPolicy::update, &post);

    ctx.response.ok(json!({ "post": post, "can_edit": can_edit }))
}
```

`can` returns a `bool` and never fails the request — for telling a client which
buttons to render.

Remember the move-order rule from chapter 05: bind `can_edit` before
`ctx.response.ok(...)`, since that call consumes part of `ctx`.

## Order matters: 404 before 403

Load the row first, authorize second:

```rust
let post = Post::find_or_fail(id).await?;   // 404 if it does not exist
ctx.authorize(PostPolicy::update, &post)?;  // 403 if it does but they may not
```

Reversing that is not possible here — you need the row to decide — but the
principle generalises: *existence* is checked before *permission*.

There is a subtlety worth naming. Returning `403` for a row that exists tells the
caller it exists. For most applications that is fine. For something where the
mere existence of a record is sensitive, return a `404` for both cases instead:

```rust
let post = Post::find(id).await?;

let Some(post) = post.filter(|p| ctx.can(PostPolicy::view, p)) else {
    return Err(Error::not_found("Post", id));
};
```

Now "does not exist" and "not yours" are indistinguishable.

## Why `bool` and not `Result`

A policy answers a permission question. Returning `Result` would invite putting
*other* failures in there — a missing row, a database error — and those are not
permission decisions. A missing row is a `404` and belongs before the check.

Keeping policies to `bool` means they stay pure, testable without a database, and
obviously correct at a glance:

```rust
#[test]
fn only_the_owner_may_update() {
    let mut auth = Auth::default();
    auth.set(Identity::new("1"));

    let mine = Post { id: 1, user_id: 1, /* ... */ };
    let theirs = Post { id: 2, user_id: 2, /* ... */ };

    assert!(PostPolicy::update(&auth, &mine));
    assert!(!PostPolicy::update(&auth, &theirs));
}
```

No HTTP, no database, no async.

## Roles

Policies read whatever is on the identity, so roles come from claims:

```rust
pub fn delete(auth: &Auth, post: &Post) -> bool {
    let Some(identity) = auth.try_identity() else {
        return false;
    };

    let is_admin = identity
        .claim::<String>("role")
        .ok()
        .flatten()
        .is_some_and(|role| role == "admin");

    is_admin || identity.id::<i64>().is_ok_and(|id| id == post.user_id)
}
```

Put the role in the token when you issue it:

```rust
let identity = Identity::new(user.id.to_string()).with_claim("role", user.role);
```

Claims travel in the token, so a role change does not take effect until the next
token is issued. For roles that must revoke immediately, read the user row
instead.

## Policies for a whole class of thing

Not every policy needs a model:

```rust
pub struct AdminPolicy;

impl AdminPolicy {
    pub fn access(auth: &Auth, _: &()) -> bool {
        auth.try_identity()
            .and_then(|i| i.claim::<String>("role").ok().flatten())
            .is_some_and(|role| role == "admin")
    }
}
```

```rust
ctx.authorize(AdminPolicy::access, &())?;
```

Though when it applies to every route in a section, middleware is tidier:

```rust
r.group("/admin", |r| {
    r.middleware(Auth::jwt());
    r.middleware(RequireRole::new("admin"));
    // ...
});
```

## Where authorization goes

| Check | Where |
|---|---|
| "must be signed in" | middleware (`Auth::jwt()`) |
| "must have this role" | middleware |
| "must own *this row*" | a policy, in the action |

The rule of thumb: if the check needs the specific record, it belongs in the
action after you have loaded it. Otherwise it belongs in middleware, where it
runs once and protects everything below.

---

Previous: [17 — Sessions](17_Sessions.md) · Next: [19 — OpenAPI](19_OpenAPI.md)
