# 14 — Scopes and Hooks

Two ways to put behaviour on a model: **scopes** name a reusable piece of a
query, **hooks** run automatically around writes.

## Scopes

You will write `where_eq(Post::published, true)` in a dozen places, and one day
change what "published" means. A scope names it once.

```rust
// src/models/post.rs
pub use crate::entities::posts::Model as Post;

use luxid::prelude::*;
use luxid::Query;

use crate::entities::posts;

#[luxid::model()]
impl Post {
    #[scope]
    fn published(query: Query<posts::Entity>) -> Query<posts::Entity> {
        query.where_eq(Post::published, true)
    }
}
```

A scope takes the query, returns the query. That is all.

### Two ways to call it

```rust
Post::published().all().await?
```

An associated function on the model that starts a query. Needs no import.

```rust
use crate::models::post::PostScopes;

Post::query().where_eq(Post::user_id, id).published().all().await?
```

Mid-chain, which needs the generated `PostScopes` trait in scope. The trait is
generated alongside the impl block, so it lives in the same module as your
model — `crate::models::post::PostScopes`, not in the entity module.

That import is the one thing people get wrong. If `.published()` does not
resolve, this is why.

### Scopes take arguments

```rust
#[scope]
fn in_team(query: Query<posts::Entity>, team_id: i64) -> Query<posts::Entity> {
    query.where_eq(Post::team_id, team_id)
}

#[scope]
fn titled_like(query: Query<posts::Entity>, pattern: &str) -> Query<posts::Entity> {
    query.where_like(Post::title, pattern)
}
```

```rust
Post::in_team(3).all().await?;
Post::titled_like("%rust%").all().await?;
```

### They compose

With everything else, in any order:

```rust
Post::published()
    .in_team(3)
    .with("author")
    .order_by_desc(Post::id)
    .paginate(1, 20)
    .await?
```

### A scope may not share a name with a column

Both become associated items on the model, so this collides:

```rust
pub done: bool,          // gives you the column `Todo::done`

#[scope]
fn done(query: ...)      // ✗ duplicate definition
```

```
error[E0592]: duplicate definitions with name `done`
```

Name the scope for the *filter* rather than the field — `completed`,
`outstanding`, `visible` — which usually reads better anyway.

### Ordinary functions are untouched

Anything in the block without `#[scope]` stays exactly as written:

```rust
#[luxid::model()]
impl Post {
    #[scope]
    fn published(query: Query<posts::Entity>) -> Query<posts::Entity> {
        query.where_eq(Post::published, true)
    }

    // Not a scope. A plain method.
    pub fn excerpt(&self) -> String {
        self.title.chars().take(40).collect()
    }
}
```

## Hooks

A hook runs automatically when a row is written. The classic use is hashing a
password so it can never be stored in plain text by accident.

Hooks are declared **on the derive**, and their functions live in a plain `impl`:

```rust
// src/entities/users.rs
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, serde::Serialize, luxid::Model)]
#[luxid(before_create = Self::hash_password)]
#[sea_orm(table_name = "users")]
pub struct Model {
    // <luxid:fields>
    #[sea_orm(primary_key)]
    pub id: i64,
    pub email: String,
    pub password: String,
    // </luxid:fields>
    #[sea_orm(ignore)]
    #[serde(flatten)]
    pub relations: luxid::Relations,
}

impl Model {
    async fn hash_password(active: &mut ActiveModel) -> luxid::Result<()> {
        if let sea_orm::ActiveValue::Set(password) = &active.password {
            active.password = sea_orm::ActiveValue::Set(luxid::Hash::make(password)?);
        }
        Ok(())
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
```

Now no code path can insert an unhashed password. Not the controller, not a
seeder, not a test.

### The six hook points

| Hook | Receives | When |
|---|---|---|
| `before_save` | `&mut ActiveModel` | before any write |
| `before_create` | `&mut ActiveModel` | before an insert |
| `before_update` | `&mut ActiveModel` | before an update |
| `after_create` | `&Model` | after an insert |
| `after_update` | `&Model` | after an update |
| `after_save` | `&Model` | after any write |

Order on create:

```
before_save → before_create → INSERT → after_create → after_save
```

Update mirrors it. Declare several at once:

```rust
#[luxid(
    before_save = Self::stamp,
    before_create = Self::hash_password,
    after_create = Self::send_welcome,
)]
```

### `before` hooks can abort the write

Return an error and nothing is written, and no `after` hook runs:

```rust
async fn reject_reserved(active: &mut ActiveModel) -> luxid::Result<()> {
    if let sea_orm::ActiveValue::Set(name) = &active.name
        && name == "admin"
    {
        return Err(luxid::Error::Conflict("that name is reserved".into()));
    }
    Ok(())
}
```

### Why hooks are declared on the derive

It looks like it would be nicer to write `#[before_save]` above the function, the
way scopes work. There is a reason it does not.

`luxid::insert` and `luxid::update` *require* the hooks trait, so hooks always
run on the ordinary write path. A hook that silently fails to fire is not an
inconvenience — it is an unhashed password in your database. Requiring the trait
means every model must implement it, which means the derive must generate it,
which means the derive has to know which hooks exist.

The cost is the function name appearing twice. The benefit is that there is no
way to write a model whose hooks quietly do not run.

### The escape hatch

```rust
luxid::insert_without_hooks(active).await?
```

Named for what it costs you. Use it in seeders and fixtures where hooks would be
wrong — never in application code.

## Which to use

| You want | Use |
|---|---|
| A filter used in several places | a scope |
| Something derived on every save | a `before` hook |
| Something to happen after a row exists | an `after` hook |
| A computed value from an existing row | a plain method |

---

Previous: [13 — Relations](13_Relations.md) · Next: [15 — Validation](15_Validation.md)
