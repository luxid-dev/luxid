# 12 — Models and Queries

## Two files per model

Luxid splits a model in two, and the split matters:

- **`src/entities/posts.rs`** — the table's shape. Generated from the database
  by `db:sync`. You do not hand-edit the field list.
- **`src/models/post.rs`** — your behaviour: relations, scopes. Yours entirely.

Keeping them apart means resyncing after a migration can never destroy the code
you wrote.

## The entity

```rust
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, serde::Serialize, luxid::Model)]
#[sea_orm(table_name = "posts")]
pub struct Model {
    // <luxid:fields>  refreshed by `cargo luxid db:sync`
    #[sea_orm(primary_key)]
    pub id: i64,
    pub title: String,
    pub published: bool,
    // </luxid:fields>
    #[sea_orm(ignore)]
    #[serde(flatten)]
    pub relations: luxid::Relations,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
```

Three parts do real work:

- **`luxid::Model`** gives you `find`, `query`, and typed columns.
- **The markers** are what `db:sync` rewrites. Nothing outside them is touched.
- **`relations`** holds eager-loaded relations. It is not a column
  (`#[sea_orm(ignore)]`) and it serializes inline (`#[serde(flatten)]`), so a
  post with its author loaded renders both together. Chapter 13.

## The model

```rust
pub use crate::entities::posts::Model as Post;

#[luxid::model()]
impl Post {}
```

`Post` is an alias for the entity's `Model`, so it has the columns as ordinary
fields. The `#[luxid::model()]` block is where relations and scopes go —
chapters 13 and 14.

## Finding rows

```rust
Post::find(id).await?           // Option<Post>
Post::find_or_fail(id).await?   // Post, or a 404
Post::all().await?              // Vec<Post>
Post::count_all().await?        // u64
```

`find_or_fail` is the one you will use most, because it makes actions short:

```rust
async fn show(ctx: HttpContext) -> Result<Response> {
    let post = Post::find_or_fail(ctx.params.get::<i64>("id")?).await?;
    ctx.response.ok(post)
}
```

A missing row produces a `404` naming the resource and id. No branching.

## Querying

```rust
let posts = Post::query()
    .where_eq(Post::published, true)
    .order_by_desc(Post::id)
    .limit(10)
    .all()
    .await?;
```

### Filtering

```rust
.where_eq(Post::published, true)
.where_ne(Post::status, "draft")
.where_gt(Post::views, 100)
.where_lt(Post::views, 1000)
.where_in(Post::status, ["published", "archived"])
.where_like(Post::title, "%rust%")
.where_null(Post::deleted_at)
.where_not_null(Post::published_at)
```

Chained conditions are combined with AND.

### Ordering and limiting

```rust
.order_by_asc(Post::title)
.order_by_desc(Post::id)
.limit(10)
.offset(20)
```

### Finishing

```rust
.all().await?              // Vec<Post>
.first().await?            // Option<Post>
.first_or_fail().await?    // Post, or a 404
.count().await?            // u64
.exists().await?           // bool
.paginate(page, 20).await? // Paginated<Post>
```

Nothing runs until one of these is called.

## Typed columns catch mistakes at compile time

`Post::published` is not a string — it is a generated type that knows the
column's Rust type:

```rust
Post::query().where_eq(Post::published, true)      // ✓ compiles
Post::query().where_eq(Post::published, "yes")     // ✗ does not compile
```

That second line is a compile error, not a runtime one. Compare with an untyped
API, where `"yes"` would be accepted and fail — or worse, silently match nothing
— at run time.

The entity's own `Column` enum remains available as an escape hatch, accepting
anything:

```rust
Post::query().where_eq(posts::Column::Published, true)
```

Reach for it only when the typed form cannot express something.

## Pagination

```rust
let page = ctx.request.input::<u64>("page")?.unwrap_or(1);
let posts = Post::query().order_by_desc(Post::id).paginate(page, 20).await?;

ctx.response.ok(posts)
```

```json
{
  "data": [ /* ... */ ],
  "page": 1,
  "per_page": 20,
  "total": 57,
  "last_page": 3
}
```

Pages are **1-based**, matching what people type in URLs. Nonsense input is
clamped rather than fatal — `paginate(0, 0)` gives you page 1 with one row per
page — and asking for a page past the end returns an empty `data` rather than an
error.

In Rust:

```rust
posts.data        // Vec<Post>
posts.total       // u64
posts.last_page   // u64
posts.has_more()  // bool
posts.len()       // usize
posts.is_empty()  // bool
```

## Writing rows

Writes go through an `ActiveModel` — a version of the struct where each field is
"set" or "unchanged".

### Inserting

```rust
use sea_orm::ActiveValue::Set;

use crate::entities::posts;

let post = luxid::insert(posts::ActiveModel {
    title: Set("Hello".to_owned()),
    published: Set(false),
    ..Default::default()
})
.await?;
```

`..Default::default()` leaves everything else unset — including `id`, which the
database assigns. The returned value is the stored row, with its id.

### Updating

```rust
use sea_orm::IntoActiveModel;

let post = Post::find_or_fail(id).await?;

let mut active = post.into_active_model();
active.title = Set("A better title".to_owned());

let post = luxid::update(active).await?;
```

Only the fields you `Set` are written.

### Deleting

```rust
use crate::entities::posts::Entity as Posts;

let removed: bool = luxid::delete_by_id::<Posts>(id).await?;
```

Returns whether anything was removed — deleting a row that is already gone is
not an error.

### Hooks run on writes

`insert` and `update` run the model's lifecycle hooks (chapter 14). There is also
`luxid::insert_without_hooks`, named for what it costs you — for seeders and
fixtures where hooks would be wrong. Never reach for it in application code: a
hook that quietly does not fire is how an unhashed password reaches the
database.

## A complete controller

```rust
use luxid::prelude::*;
use sea_orm::ActiveValue::Set;
use serde_json::Value;

use crate::entities::posts;
use crate::models::post::Post;

pub struct PostsController;

#[luxid::controller]
impl PostsController {
    async fn index(ctx: HttpContext) -> Result<Response> {
        let page = ctx.request.input::<u64>("page")?.unwrap_or(1);

        let posts = Post::query()
            .where_eq(Post::published, true)
            .order_by_desc(Post::id)
            .paginate(page, 20)
            .await?;

        ctx.response.ok(posts)
    }

    async fn show(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(Post::find_or_fail(ctx.params.get::<i64>("id")?).await?)
    }

    async fn store(ctx: HttpContext) -> Result<Response> {
        let body: Value = ctx.request.body_json()?;
        let title = body.get("title").and_then(Value::as_str).unwrap_or_default();

        let post = luxid::insert(posts::ActiveModel {
            title: Set(title.to_owned()),
            published: Set(false),
            ..Default::default()
        })
        .await?;

        ctx.response.created(post)
    }

    async fn destroy(ctx: HttpContext) -> Result<Response> {
        let id: i64 = ctx.params.get("id")?;
        Post::find_or_fail(id).await?;

        luxid::delete_by_id::<posts::Entity>(id).await?;
        ctx.response.no_content()
    }
}
```

`store` reads the body by hand there, which chapter 15 replaces with something
much better.

---

Previous: [11 — Database and Migrations](11_Database_and_Migrations.md) · Next: [13 — Relations](13_Relations.md)
