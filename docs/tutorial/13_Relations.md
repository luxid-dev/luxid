# 13 — Relations

Rows reference other rows: a post has an author, a user has many posts. This
chapter covers declaring those links, loading them efficiently, and the mistake
that makes web applications slow.

## The N+1 problem

Say you list twenty posts and show each author's name. The naive approach:

```
SELECT * FROM posts LIMIT 20          -- 1 query
SELECT * FROM users WHERE id = 1      -- then one per post
SELECT * FROM users WHERE id = 2
... eighteen more
```

Twenty-one queries for twenty posts. At a hundred posts it is a hundred and one.
This is the **N+1 problem**, and it is the single most common cause of slow
endpoints in every framework.

The fix is to fetch all the authors in one query. Luxid calls that **eager
loading**, and — importantly — it makes forgetting to do so an error rather than
a slow page.

## Declaring relations

In `src/models/post.rs`:

```rust
pub use crate::entities::posts::Model as Post;

use crate::models::user::User;

#[luxid::model(belongs_to(author = User, fk = "user_id"))]
impl Post {}
```

And the other direction, in `src/models/user.rs`:

```rust
pub use crate::entities::users::Model as User;

use crate::models::post::Post;

#[luxid::model(has_many(posts = Post, fk = "user_id"))]
impl User {}
```

Read those as sentences: *a post belongs to an author, found via the `user_id`
column*; *a user has many posts, found via `posts.user_id`*.

### The three kinds

```rust
#[luxid::model(
    has_many(posts = Post, fk = "user_id"),        // one user → many posts
    has_one(profile = Profile, fk = "user_id"),    // one user → one profile
    belongs_to(team = Team),                       // this row holds team_id
)]
impl User {}
```

**`has_many`** and **`has_one`** — the *other* table holds the foreign key, so
you must name it with `fk`.

**`belongs_to`** — *this* table holds it, and the name is inferred from the
relation: `belongs_to(team = Team)` looks for `team_id`. Override when it
differs:

```rust
belongs_to(author = User, fk = "user_id")
```

Both sides accept `local_key` when the joined column is not `id`.

## Loading and reading them

```rust
let posts = Post::query().with("author").paginate(1, 20).await?;

for post in &posts.data {
    let author = post.author()?;      // Option<&User>
}
```

```rust
let users = User::query().with("posts").all().await?;

for user in &users {
    let posts = user.posts()?;        // &[Post]
}
```

Each relation generates a **method named after it**. That is why two relations
pointing at the same model stay unambiguous:

```rust
#[luxid::model(
    belongs_to(author = User, fk = "author_id"),
    belongs_to(editor = User, fk = "editor_id"),
)]
impl Post {}
```

```rust
post.author()?    // Option<&User>
post.editor()?    // Option<&User>
```

Load several at once:

```rust
Post::query().with("author").with("comments").all().await?
```

## One query per relation, whatever the page size

`.with("author")` on twenty posts issues **one** query for the authors:

```
SELECT * FROM posts LIMIT 20
SELECT * FROM users WHERE id IN (1, 2, 3)
```

Two queries, not twenty-one. Duplicate keys are collapsed first, so a hundred
posts by three authors fetch three rows.

## Relations serialize with the model

A loaded relation appears in the JSON alongside the columns:

```rust
let post = Post::query().with("author").first_or_fail().await?;
ctx.response.ok(post)
```

```json
{
  "id": 1,
  "title": "Hello",
  "user_id": 7,
  "author": { "id": 7, "name": "Ada" }
}
```

A model with nothing loaded renders no relation keys at all — you never get
`"author": null` for a relation you simply did not ask for.

## Forgetting to load is an error

This is the part that saves you.

```rust
let posts = Post::query().all().await?;   // no .with("author")
let author = posts[0].author()?;          // ← Err
```

```
the `author` relation of `Post` was not loaded.
Add `.with("author")` to the query, or call
`luxid::set_strict_relations(false)` to read unloaded relations as empty.
```

The message names the exact fix. And because it is an error rather than a silent
extra query, **an N+1 becomes a failing test** instead of a production
slowdown.

This is on in development and off in release, controlled by `luxid.toml`:

```toml
[database]
strict_relations = true
```

Leave it on in tests. That is where it earns its keep.

A parent with no children is *loaded and empty*, not unloaded — a user with zero
posts gives you `[]`, not an error. Only genuinely forgetting to load trips it.

## A misspelled relation says what exists

```rust
Post::query().with("auther").all().await?
```

```
`Post` has no relation `auther`. Declared relations: [author, comments].
```

## Current limits

Two things to know before you design around this:

**Eager paths are single-level.** `.with("posts.comments")` does not work yet —
it reports the relation as undeclared. Load one level, then query the second.

**`.with()` needs a declared relation.** A model whose `#[luxid::model()]` block
declares none cannot be passed to `.with()` at all — that is a compile error, not
a runtime surprise.

## A worked example

```rust
// src/models/user.rs
pub use crate::entities::users::Model as User;

use crate::models::post::Post;

#[luxid::model(has_many(posts = Post, fk = "user_id"))]
impl User {}
```

```rust
// src/controllers/users_controller.rs
async fn show(ctx: HttpContext) -> Result<Response> {
    let id: i64 = ctx.params.get("id")?;

    let user = User::query()
        .where_eq(User::id, id)
        .with("posts")
        .first_or_fail()
        .await?;

    ctx.response.ok(user)
}
```

```json
{
  "id": 7,
  "name": "Ada",
  "posts": [
    { "id": 1, "title": "Hello", "user_id": 7 },
    { "id": 4, "title": "Again", "user_id": 7 }
  ]
}
```

Two queries, one endpoint, and the relation is impossible to forget without the
tests telling you.

---

Previous: [12 — Models and Queries](12_Models_and_Queries.md) · Next: [14 — Scopes and Hooks](14_Scopes_and_Hooks.md)
