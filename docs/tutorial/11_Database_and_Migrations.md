# 11 — Database and Migrations

## Connecting

A generated app connects in `src/app.rs`:

```rust
let url = config.get_or("database.url", "sqlite://./app.db?mode=rwc".to_owned())?;
let db = Db::connect(url).await?;
```

The default is SQLite in a file next to your code, so a fresh project runs with
nothing installed. Point `DATABASE_URL` at Postgres when you want one:

```sh
DATABASE_URL=postgres://user:password@localhost/blog
```

Nothing else changes. Both are supported throughout.

The connection is registered as a singleton and made available to requests by
the `WithDatabase` middleware:

```rust
Ok(App::new()
    .providers(Providers::new().singleton(move |_| db.clone()))
    .middleware(WithDatabase)
    .routes(crate::routes::register))
```

If you forget `WithDatabase`, queries fail with a message saying so. They do not
silently use the wrong connection.

## How queries find the connection

You will notice that queries do not take a database argument:

```rust
let posts = Post::query().all().await?;
```

The connection is *ambient* — the middleware puts it in scope for the duration of
the request, and queries pick it up. This is what lets model code read like
`User::find(id)` instead of `User::find(&db, id)`.

Two consequences worth knowing:

- Code outside a request needs its own scope: `db.scope(async { ... }).await`.
- A detached `tokio::spawn` does **not** inherit the scope. Queries there fail
  with a message explaining exactly that, rather than quietly using a different
  connection.

## What migrations are

A migration is a versioned, repeatable change to your database structure. You do
not create tables by hand — you write a migration, commit it, and every
environment applies the same ones in the same order.

## Creating one

```sh
luxid make:model Post -m
```

That writes `migration/src/m20260822_140530_create_posts.rs`:

```rust
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveIden)]
enum Posts {
    Table,
    Id,
}

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260822_140530_create_posts"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Posts::Table)
                    .if_not_exists()
                    .col(pk_auto(Posts::Id))
                    // Add your columns here.
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.drop_table(Table::drop().table(Posts::Table).to_owned()).await
    }
}
```

Note there is **no `--fields` flag**. The migration starts empty and you fill in
the columns. Your database is the source of truth for your schema, and a field
DSL would be a second, weaker one that cannot express every column type.

## Filling it in

Add the column names to the enum, then the columns to the table:

```rust
#[derive(DeriveIden)]
enum Posts {
    Table,
    Id,
    Title,
    Body,
    Published,
}
```

```rust
.col(pk_auto(Posts::Id))
.col(string(Posts::Title))
.col(text(Posts::Body))
.col(boolean(Posts::Published))
```

Common column helpers:

| Helper | Column |
|---|---|
| `pk_auto(X)` | auto-incrementing primary key |
| `string(X)` / `string_null(X)` | VARCHAR, required / nullable |
| `text(X)` / `text_null(X)` | TEXT |
| `integer(X)` / `big_integer(X)` | INTEGER / BIGINT |
| `boolean(X)` | BOOLEAN |
| `timestamp(X)` / `timestamp_null(X)` | TIMESTAMP |
| `double(X)` / `decimal(X)` | floating point / exact decimal |

Every `*_null` variant makes the column optional.

### Foreign keys

```rust
#[derive(DeriveIden)]
enum Posts {
    Table,
    Id,
    UserId,
}
```

```rust
.col(big_integer(Posts::UserId))
.foreign_key(
    ForeignKey::create()
        .from(Posts::Table, Posts::UserId)
        .to(Users::Table, Users::Id)
        .on_delete(ForeignKeyAction::Cascade),
)
```

Referencing another table means declaring its identifier too:

```rust
#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
```

## Running them

```sh
cargo luxid migrate            # apply everything pending
cargo luxid migrate:status     # what has run
cargo luxid migrate:rollback   # undo the last one
cargo luxid migrate:fresh --force   # drop everything and rebuild
```

`migrate:fresh` requires `--force` because it destroys data, and that should not
follow from a mistyped command in the wrong shell.

`migrate:status` is worth checking when behaviour differs between machines:

```
 applied  m20260822_140530_create_posts
 pending  m20260823_101500_add_published_to_posts
```

## One migration per file

SeaORM derives a migration's name from its **file name**, not its struct name.
Two migrations in one file therefore share a name, and the second is silently
treated as already applied — which is a data-loss-shaped trap.

`luxid make:model -m` writes one per file, correctly named. If you write one by
hand, keep that rule, or implement `MigrationName` explicitly as the generated
ones do.

## Changing an existing table

There is no generator for this yet — write the file by hand in
`migration/src/`, named with a later timestamp, and register it in
`migration/src/lib.rs`:

```rust
mod m20260822_140530_create_posts;
mod m20260823_101500_add_published_to_posts;   // ← add

// <luxid:migration-modules>

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260822_140530_create_posts::Migration),
            Box::new(m20260823_101500_add_published_to_posts::Migration),   // ← add
            // <luxid:migrations>
        ]
    }
}
```

Order matters: they run top to bottom.

## Keeping code in step with the schema

After a migration, your Rust code needs to know about the new columns:

```sh
cargo luxid db:sync
```

That reads the **live database** and refreshes the field lists in your entities
and factories — but only what lies between the `// <luxid:fields>` markers. Rules
and overrides you wrote outside them survive.

```
  updated src/entities/posts.rs
  updated src/factories/post_factory.rs
1 table(s) read, 2 file(s) changed
```

Use `--dry-run` to see what would change first. Running it twice changes nothing
the second time.

The usual loop is therefore:

```sh
luxid make:model Post -a     # generate
# edit the migration to add columns
cargo luxid migrate         # apply
cargo luxid db:sync         # bring the code into step
```

## Transactions

The `Db` handle itself is a service, so resolve it when you need one:

```rust
let db = ctx.services.get::<Db>()?;

db.transaction(async || {
    let user = luxid::insert(new_user).await?;
    luxid::insert(new_profile(user.id)).await?;
    Ok(())
})
.await?;
```

Commits on `Ok`, rolls back on `Err`. Every query inside joins the transaction
automatically — there is no handle to thread through.

---

Previous: [10 — Configuration](10_Configuration.md) · Next: [12 — Models and Queries](12_Models_and_Queries.md)
