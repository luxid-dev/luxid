# 23 — Project: a Todo API

The second project adds what chapter 22 did not need: relations, ownership,
scopes, filtering, and pagination. It continues from the auth app — same
project, same users.

Everything here assumes you finished chapter 22.

## 1. Scaffold the todo

```sh
luxid make:model Todo -a
```

## 2. The table

`migration/src/m<timestamp>_create_todos.rs`:

```rust
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveIden)]
enum Todos {
    Table,
    Id,
    UserId,
    Title,
    Notes,
    Done,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260822_130000_create_todos"      // keep whatever was generated
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Todos::Table)
                    .if_not_exists()
                    .col(pk_auto(Todos::Id))
                    .col(big_integer(Todos::UserId))
                    .col(string(Todos::Title))
                    .col(text_null(Todos::Notes))
                    .col(boolean(Todos::Done))
                    .foreign_key(
                        ForeignKey::create()
                            .from(Todos::Table, Todos::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.drop_table(Table::drop().table(Todos::Table).to_owned()).await
    }
}
```

`ForeignKeyAction::Cascade` means deleting a user deletes their todos. The
alternative — orphaned rows pointing at a user that no longer exists — is worse.

Note `Users` is declared again here. Each migration is self-contained.

```sh
cargo run -- migrate
cargo run -- db:sync
```

## 3. Relations

`src/models/todo.rs`:

```rust
pub use crate::entities::todos::Model as Todo;

use luxid::Query;
use luxid::prelude::*;

use crate::entities::todos;
use crate::models::user::User;

#[luxid::model(belongs_to(owner = User, fk = "user_id"))]
impl Todo {
    // Named `completed`, not `done`: a scope may not share a name with a
    // column, and `Todo::done` is one. See chapter 14.
    #[scope]
    fn completed(query: Query<todos::Entity>) -> Query<todos::Entity> {
        query.where_eq(Todo::done, true)
    }

    #[scope]
    fn outstanding(query: Query<todos::Entity>) -> Query<todos::Entity> {
        query.where_eq(Todo::done, false)
    }

    #[scope]
    fn owned_by(query: Query<todos::Entity>, user_id: i64) -> Query<todos::Entity> {
        query.where_eq(Todo::user_id, user_id)
    }
}
```

`owned_by` is the important one. Every query in this API filters by owner, and
naming it once means no endpoint can forget — and if the ownership rule ever
changes, it changes in one place.

The other side, `src/models/user.rs`:

```rust
pub use crate::entities::users::Model as User;

use crate::models::todo::Todo;

#[luxid::model(has_many(todos = Todo, fk = "user_id"))]
impl User {}
```

## 4. The ownership policy

`src/policies/todo_policy.rs`:

```rust
use luxid::prelude::*;

use crate::models::todo::Todo;

pub struct TodoPolicy;

impl TodoPolicy {
    pub fn owns(auth: &Auth, todo: &Todo) -> bool {
        auth.try_identity()
            .and_then(|identity| identity.id::<i64>().ok())
            .is_some_and(|id| id == todo.user_id)
    }
}
```

One rule, used by every endpoint that touches a specific todo.

## 5. Validation

`src/validators/todo.rs`:

```rust
use luxid::prelude::*;
use serde::Deserialize;

#[derive(Debug, Deserialize, Validate, luxid::JsonSchema)]
pub struct StoreTodo {
    #[validate(length(min = 1, max = 200))]
    pub title: String,

    #[validate(length(max = 2000))]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize, Validate, luxid::JsonSchema)]
pub struct UpdateTodo {
    #[validate(length(min = 1, max = 200))]
    pub title: Option<String>,

    #[validate(length(max = 2000))]
    pub notes: Option<String>,

    pub done: Option<bool>,
}
```

Every field on `UpdateTodo` is optional, so a client can send just
`{"done": true}` without resending the title. The rules still apply to whatever
*is* sent.

## 6. The controller

`src/controllers/todos_controller.rs`:

```rust
use luxid::prelude::*;
use sea_orm::ActiveValue::Set;
use sea_orm::IntoActiveModel;

use crate::entities::todos;
use crate::models::todo::{Todo, TodoScopes};
use crate::policies::todo_policy::TodoPolicy;
use crate::validators::todo::{StoreTodo, UpdateTodo};

pub struct TodosController;

#[luxid::controller]
impl TodosController {
    #[openapi(summary = "List your todos", tag = "todos", errors = [401])]
    async fn index(ctx: HttpContext) -> Result<Response> {
        let user_id: i64 = ctx.auth.id()?;
        let page = ctx.request.input::<u64>("page")?.unwrap_or(1);
        let per_page = ctx.request.input::<u64>("per_page")?.unwrap_or(20).min(100);

        let mut query = Todo::owned_by(user_id);

        // ?status=done | pending — anything else means no filter.
        query = match ctx.request.input::<String>("status")?.as_deref() {
            Some("done") => query.completed(),
            Some("pending") => query.outstanding(),
            _ => query,
        };

        let todos = query.order_by_desc(Todo::id).paginate(page, per_page).await?;

        ctx.response.ok(todos)
    }

    #[openapi(summary = "One todo", tag = "todos", errors = [401, 404])]
    async fn show(ctx: HttpContext) -> Result<Response> {
        let todo = Self::find_owned(&ctx).await?;

        ctx.response.ok(todo)
    }

    #[openapi(summary = "Create a todo", tag = "todos", body = StoreTodo, errors = [401, 422])]
    async fn store(ctx: HttpContext) -> Result<Response> {
        let user_id: i64 = ctx.auth.id()?;
        let input = ctx.request.validate::<StoreTodo>().await?;

        let todo = luxid::insert(todos::ActiveModel {
            user_id: Set(user_id),
            title: Set(input.title),
            notes: Set(input.notes),
            done: Set(false),
            ..Default::default()
        })
        .await?;

        ctx.response.created(todo)
    }

    #[openapi(summary = "Update a todo", tag = "todos", body = UpdateTodo, errors = [401, 404, 422])]
    async fn update(ctx: HttpContext) -> Result<Response> {
        let todo = Self::find_owned(&ctx).await?;
        let input = ctx.request.validate::<UpdateTodo>().await?;

        let mut active = todo.into_active_model();

        if let Some(title) = input.title {
            active.title = Set(title);
        }
        if let Some(notes) = input.notes {
            active.notes = Set(Some(notes));
        }
        if let Some(done) = input.done {
            active.done = Set(done);
        }

        ctx.response.ok(luxid::update(active).await?)
    }

    #[openapi(summary = "Delete a todo", tag = "todos", no_content, errors = [401, 404])]
    async fn destroy(ctx: HttpContext) -> Result<Response> {
        let todo = Self::find_owned(&ctx).await?;

        luxid::delete_by_id::<todos::Entity>(todo.id).await?;

        ctx.response.no_content()
    }

    /// Load a todo the caller is allowed to see.
    ///
    /// Returns a 404 rather than a 403 for someone else's todo: replying "you
    /// may not touch this" would confirm that it exists.
    async fn find_owned(ctx: &HttpContext) -> Result<Todo> {
        let id: i64 = ctx.params.get("id")?;
        let todo = Todo::find(id).await?;

        let Some(todo) = todo.filter(|todo| ctx.can(TodoPolicy::owns, todo)) else {
            return Err(Error::not_found("Todo", id));
        };

        Ok(todo)
    }
}
```

Two things to notice.

**`find_owned` is not an action.** The controller macro only treats an
`async fn` as an action when its single argument is an `HttpContext` *by value*.
This one takes `&HttpContext`, so it stays an ordinary helper — which is how you
share logic between actions.

**404, not 403, for someone else's todo.** Chapter 18 raised this: a `403` on a
row that exists confirms it exists. For a todo list that leaks nothing much; for
anything sensitive it matters, and the habit is worth forming.

## 7. Routes

Add to the guarded group in `src/routes.rs`:

```rust
r.group("/", |r| {
    r.middleware(Auth::jwt());

    r.get("/me", controllers::auth_controller::AuthController::me);
    r.resource("/todos", controllers::todos_controller::TodosController);
});
```

One line for five routes. Declare the module in `src/controllers/mod.rs`:

```rust
pub mod todos_controller;
```

## 8. Try it

```sh
cargo run
```

```sh
TOKEN=$(curl -s -X POST localhost:3000/api/login -H 'content-type: application/json' \
  -d '{"email":"ada@example.com","password":"hunter2hunter2"}' | jq -r .token)

AUTH="authorization: Bearer $TOKEN"

curl -X POST localhost:3000/api/todos -H "$AUTH" -H 'content-type: application/json' \
  -d '{"title":"Write the docs"}'

curl -X POST localhost:3000/api/todos -H "$AUTH" -H 'content-type: application/json' \
  -d '{"title":"Ship 0.2","notes":"after the docs"}'

curl -H "$AUTH" localhost:3000/api/todos
```

```json
{
  "data": [
    { "id": 2, "user_id": 1, "title": "Ship 0.2", "notes": "after the docs", "done": false },
    { "id": 1, "user_id": 1, "title": "Write the docs", "notes": null, "done": false }
  ],
  "page": 1,
  "per_page": 20,
  "total": 2,
  "last_page": 1
}
```

Mark one done and filter:

```sh
curl -X PUT localhost:3000/api/todos/1 -H "$AUTH" -H 'content-type: application/json' \
  -d '{"done":true}'

curl -H "$AUTH" 'localhost:3000/api/todos?status=pending'
curl -H "$AUTH" 'localhost:3000/api/todos?status=done'
```

And confirm the guard:

```sh
curl localhost:3000/api/todos        # 401
```

## 9. Loading the owner

To include the owner in a response:

```rust
let todos = Todo::owned_by(user_id)
    .with("owner")
    .paginate(page, per_page)
    .await?;
```

```json
{ "id": 1, "title": "Write the docs", "owner": { "id": 1, "name": "Ada" } }
```

One extra query for the whole page, not one per row. And if you forget the
`.with("owner")` but call `todo.owner()?`, development tells you:

```
the `owner` relation of `Todo` was not loaded. Add `.with("owner")` to the query.
```

That is the N+1 protection from chapter 13 doing its job.

## 10. Tests

`tests/todos.rs`:

```rust
use luxid::prelude::*;
use luxid_testing::TestApp;
use sea_orm::ActiveValue::Set;
use serde_json::json;

use authdemo::entities::{todos, users};

const SECRET: &str = "test-signing-key";

pub async fn database() -> Db {
    let db = Db::in_memory().await.expect("opens");
    db.migrate::<migration::Migrator>().await.expect("migrates");
    db
}

fn app(db: Db) -> TestApp {
    TestApp::new(
        App::new()
            .providers(
                Providers::new()
                    .singleton(move |_| db.clone())
                    .singleton(|_| Jwt::new(SECRET)),
            )
            .middleware(WithDatabase)
            .routes(authdemo::routes::register)
            .into_service(),
    )
}

async fn a_user(email: &str) -> Result<users::Model> {
    luxid::insert(users::ActiveModel {
        name: Set("Test".to_owned()),
        email: Set(email.to_owned()),
        password: Set("hunter2hunter2".to_owned()),
        ..Default::default()
    })
    .await
}

async fn a_todo(user_id: i64, title: &str, done: bool) -> Result<todos::Model> {
    luxid::insert(todos::ActiveModel {
        user_id: Set(user_id),
        title: Set(title.to_owned()),
        notes: Set(None),
        done: Set(done),
        ..Default::default()
    })
    .await
}

#[luxid::test(db = crate::database)]
async fn listing_shows_only_your_own(db: Db) -> Result<()> {
    let mine = a_user("mine@example.com").await?;
    let theirs = a_user("theirs@example.com").await?;

    a_todo(mine.id, "Mine", false).await?;
    a_todo(theirs.id, "Theirs", false).await?;

    app(db)
        .get("/api/todos")
        .acting_as(SECRET, mine.id)
        .send()
        .await
        .assert_ok()
        .assert_json_count("data", 1)
        .assert_json_path("data.0.title", "Mine");

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn the_status_filter_works(db: Db) -> Result<()> {
    let user = a_user("ada@example.com").await?;

    a_todo(user.id, "Done", true).await?;
    a_todo(user.id, "Pending", false).await?;

    let app = app(db);

    app.get("/api/todos?status=done")
        .acting_as(SECRET, user.id)
        .send()
        .await
        .assert_json_count("data", 1)
        .assert_json_path("data.0.title", "Done");

    app.get("/api/todos?status=pending")
        .acting_as(SECRET, user.id)
        .send()
        .await
        .assert_json_count("data", 1)
        .assert_json_path("data.0.title", "Pending");

    app.get("/api/todos")
        .acting_as(SECRET, user.id)
        .send()
        .await
        .assert_json_count("data", 2);

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn creating_assigns_the_caller_as_owner(db: Db) -> Result<()> {
    let user = a_user("ada@example.com").await?;

    app(db)
        .post("/api/todos")
        .acting_as(SECRET, user.id)
        .json(json!({ "title": "Write the docs" }))
        .send()
        .await
        .assert_created()
        .assert_json_path("user_id", user.id)
        .assert_json_path("done", false);

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn a_partial_update_leaves_the_rest_alone(db: Db) -> Result<()> {
    let user = a_user("ada@example.com").await?;
    let todo = a_todo(user.id, "Original", false).await?;

    app(db)
        .put(&format!("/api/todos/{}", todo.id))
        .acting_as(SECRET, user.id)
        .json(json!({ "done": true }))
        .send()
        .await
        .assert_ok()
        .assert_json_path("done", true)
        .assert_json_path("title", "Original");

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn someone_elses_todo_is_invisible(db: Db) -> Result<()> {
    let mine = a_user("mine@example.com").await?;
    let theirs = a_user("theirs@example.com").await?;
    let todo = a_todo(theirs.id, "Theirs", false).await?;

    let app = app(db);
    let path = format!("/api/todos/{}", todo.id);

    // A 404, not a 403 — replying "forbidden" would confirm it exists.
    app.get(&path).acting_as(SECRET, mine.id).send().await.assert_not_found();

    app.put(&path)
        .acting_as(SECRET, mine.id)
        .json(json!({ "done": true }))
        .send()
        .await
        .assert_not_found();

    app.delete(&path).acting_as(SECRET, mine.id).send().await.assert_not_found();

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn deleting_removes_it(db: Db) -> Result<()> {
    let user = a_user("ada@example.com").await?;
    let todo = a_todo(user.id, "Temporary", false).await?;

    let app = app(db);
    let path = format!("/api/todos/{}", todo.id);

    app.delete(&path).acting_as(SECRET, user.id).send().await.assert_no_content();
    app.get(&path).acting_as(SECRET, user.id).send().await.assert_not_found();

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn a_title_is_required(db: Db) -> Result<()> {
    let user = a_user("ada@example.com").await?;

    app(db)
        .post("/api/todos")
        .acting_as(SECRET, user.id)
        .json(json!({ "title": "" }))
        .send()
        .await
        .assert_validation_errors(&["title"]);

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn every_route_needs_a_token(db: Db) -> Result<()> {
    let app = app(db);

    app.get("/api/todos").send().await.assert_unauthorized();
    app.post("/api/todos").json(json!({ "title": "x" })).send().await.assert_unauthorized();
    app.get("/api/todos/1").send().await.assert_unauthorized();

    Ok(())
}
```

```sh
cargo test
```

Note what the ownership tests assert: not just that the happy path works, but
that **another user gets nothing**. Ownership bugs are the most common serious
flaw in this kind of API, and they only show up in tests that use two users.

## What you built

- A resource with five routes from one `r.resource(...)` line
- Ownership enforced by a scope on lists and a policy on individual rows
- Partial updates that only touch what was sent
- Filtering via query parameters and named scopes
- A test suite that proves users cannot see each other's data

## Where to go next

- **Relations**: give todos a `Category`, load with `.with()`
- **Sessions**: add `Auth::session()` alongside the token guard for a browser
  client — `ctx.auth` reads the same either way
- **OpenAPI**: `cargo run -- openapi --pretty` and load it into Swagger UI
- **Postgres**: change `DATABASE_URL`; nothing else

---

Previous: [22 — Project: an Auth API](22_Project_Auth_App.md) · [Back to the index](README.md)
