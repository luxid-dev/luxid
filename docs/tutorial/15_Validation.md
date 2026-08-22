# 15 — Validation

Never trust input. This chapter replaces the hand-rolled checking from chapter
12 with something declarative, and introduces the rules that make Luxid's
validation unusual: ones that consult the database.

## A form request

A struct describing what the endpoint accepts, with the rules attached:

```rust
// src/validators/user.rs
use luxid::prelude::*;
use serde::Deserialize;

use crate::models::user::User;

#[derive(Debug, Deserialize, Validate, luxid::JsonSchema)]
pub struct StoreUser {
    #[validate(length(min = 2, max = 64))]
    pub name: String,

    #[validate(email, unique(User::email))]
    pub email: String,

    #[validate(length(min = 8))]
    pub password: String,
}
```

Use it in an action:

```rust
async fn store(ctx: HttpContext) -> Result<Response> {
    let input = ctx.request.validate::<StoreUser>().await?;

    // Past this line, `input` is valid. Nothing else to check.
    ctx.response.created(json!({ "name": input.name }))
}
```

One line replaces the entire block of `body.get(...).and_then(...)` from earlier.
The `?` turns any failure into a `422` listing every problem.

The `luxid::JsonSchema` derive is optional and only needed if you want this type
in your OpenAPI document (chapter 19).

## The rules

### Length — strings

```rust
#[validate(length(min = 2))]
#[validate(length(max = 64))]
#[validate(length(min = 2, max = 64))]
#[validate(length(equal = 6))]
```

Counted in **characters, not bytes** — "café" is four characters, and a user
counting them agrees.

### Email

```rust
#[validate(email)]
```

A pragmatic shape check, not RFC 5322. Full compliance accepts addresses no mail
system will deliver to and rejects nothing users actually type; every framework
that tries ends up with a regex nobody can read. If an address must genuinely
work, send a confirmation link.

### Range — numbers

```rust
#[validate(range(min = 18))]
#[validate(range(min = 18, max = 120))]
```

### Custom

```rust
fn not_reserved(name: &String) -> bool {
    !matches!(name.as_str(), "admin" | "root")
}
```

```rust
#[validate(custom(function = not_reserved, message = "is reserved"))]
pub name: String,
```

The function takes a reference to the field and returns `bool`.

### Custom messages

Any rule accepts one:

```rust
#[validate(length(min = 8, message = "pick something longer"))]
```

## Rules that hit the database

These are the ones no other Rust framework ships.

### `unique`

```rust
#[validate(email, unique(User::email))]
pub email: String,
```

Fails with *"has already been taken"* if a row already holds that value. For an
update, exclude the row being edited:

```rust
#[derive(Deserialize, Validate)]
pub struct UpdateUser {
    pub id: i64,

    #[validate(email, unique(User::email, except = "id"))]
    pub email: String,
}
```

`except` names a field **on this struct** holding the id to skip.

### `exists`

```rust
#[validate(exists(Team::id))]
pub team_id: i64,
```

Fails with *"does not exist"* if nothing matches. Use it for foreign keys, so a
bad reference becomes a clean `422` rather than a database constraint error
surfacing as a `500`.

## How the two kinds interact

Synchronous rules run first. Then the asynchronous ones run — **skipping any
field that already failed**.

That ordering matters. Send a malformed email and you get:

```json
{ "errors": { "email": ["must be a valid email address"] } }
```

not:

```json
{ "errors": { "email": ["must be a valid email address", "has already been taken"] } }
```

One mistake, one message. There is no point asking the database whether a
malformed address is taken, and reporting both would be confusing.

Fields that passed their synchronous rules still get their database rules in the
**same pass**, so a form with three database-backed rules costs one round of
queries — not three requests to discover three problems.

## Everything at once

```rust
#[derive(Deserialize, Validate)]
pub struct StoreUser {
    #[validate(length(min = 2, max = 64))]
    pub name: String,

    #[validate(email, unique(User::email))]
    pub email: String,

    #[validate(exists(Team::id))]
    pub team_id: i64,

    #[validate(range(min = 18, max = 120))]
    pub age: Option<i64>,
}
```

```sh
curl -X POST localhost:3000/api/users \
  -d '{"name":"G","email":"nope","team_id":999,"age":5}'
```

```json
{
  "type": "https://luxid.rs/errors/validation",
  "title": "The given data was invalid",
  "status": 422,
  "errors": {
    "name": ["must be at least 2 characters"],
    "email": ["must be a valid email address"],
    "team_id": ["does not exist"],
    "age": ["must be at least 18"]
  }
}
```

Four problems, one response. A client can fix the whole form in one pass.

## Optional fields

An `Option` field is validated **only when present**:

```rust
#[validate(range(min = 18, max = 120))]
pub age: Option<i64>,
```

Absent → no rule applies. Present → the range applies. Presence itself is a
different question: make the field non-`Option` and serde will reject a body
that omits it.

## Malformed bodies are a 400

```sh
curl -X POST localhost:3000/api/users -d 'not json at all'
```

gives `400`, not `422`. A `422` says "these fields are wrong", which implies the
client can fix them one at a time. A body that is not JSON is broken as a whole.

## Where validators live

```
src/validators/
├── mod.rs
├── user.rs      StoreUser, UpdateUser
└── post.rs      StorePost, UpdatePost
```

`luxid make:model User -a` generates the file with both structs and empty rule
lists. `cargo run -- db:sync` can refresh the field list from the schema, and
touches only what lies between the markers — the rules you wrote survive.

## Building errors by hand

Occasionally a rule does not fit the declarative form:

```rust
async fn store(ctx: HttpContext) -> Result<Response> {
    let input = ctx.request.validate::<StoreBooking>().await?;

    if input.ends_at <= input.starts_at {
        let mut errors = ValidationErrors::new();
        errors.add("ends_at", "must be after the start time");

        return Err(Error::Validation(errors));
    }

    // ...
}
```

The client sees the same shape either way.

---

Previous: [14 — Scopes and Hooks](14_Scopes_and_Hooks.md) · Next: [16 — Authentication](16_Authentication.md)
