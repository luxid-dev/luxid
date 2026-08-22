//! The test harness dogfooded: `TestApp` for requests, `#[luxid::test]` for
//! per-test transaction rollback.

use luxid::prelude::*;
use luxid_testing::TestApp;
use sea_orm::{ActiveValue::Set, ConnectionTrait};
use serde_json::{Value, json};

mod posts {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, serde::Serialize, luxid::Model)]
    #[sea_orm(table_name = "posts")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub title: String,

        #[sea_orm(ignore)]
        #[serde(flatten)]
        pub relations: luxid::Relations,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

use posts::Model as Post;

pub struct PostsController;

#[luxid::controller]
impl PostsController {
    async fn index(ctx: HttpContext) -> Result<Response> {
        ctx.response
            .ok(Post::query().order_by_asc(Post::id).paginate(1, 10).await?)
    }

    async fn show(ctx: HttpContext) -> Result<Response> {
        ctx.response
            .ok(Post::find_or_fail(ctx.params.get::<i64>("id")?).await?)
    }

    async fn store(ctx: HttpContext) -> Result<Response> {
        let body: Value = ctx.request.body_json()?;
        let title = body
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if title.is_empty() {
            let mut errors = ValidationErrors::new();
            errors.add("title", "is required");
            return Err(Error::Validation(errors));
        }

        let created = luxid::insert(posts::ActiveModel {
            title: Set(title.to_owned()),
            ..Default::default()
        })
        .await?;

        ctx.response.created(created)
    }
}

const SCHEMA: &str = "
    CREATE TABLE posts (
        id    INTEGER PRIMARY KEY AUTOINCREMENT,
        title TEXT    NOT NULL
    );
";

/// The factory `#[luxid::test(db = ..)]` calls. In a real app this would open
/// one shared database and run migrations once.
pub async fn database() -> Db {
    let db = Db::in_memory().await.expect("opens");
    db.connection()
        .execute_unprepared(SCHEMA)
        .await
        .expect("creates schema");

    db.scope(async {
        for title in ["First", "Second"] {
            luxid::insert(posts::ActiveModel {
                title: Set(title.to_owned()),
                ..Default::default()
            })
            .await
            .expect("seeds");
        }
    })
    .await;

    db
}

fn app(db: Db) -> TestApp {
    TestApp::new(
        App::new()
            .providers(Providers::new().singleton(move |_| db.clone()))
            .middleware(WithDatabase)
            .routes(|r| {
                r.group("/api", |r| {
                    r.get("/posts", PostsController::index);
                    r.post("/posts", PostsController::store);
                    r.get("/posts/{id}", PostsController::show);
                });
            })
            .into_service(),
    )
}

#[luxid::test(db = crate::database)]
async fn the_index_is_paginated(db: Db) -> Result<()> {
    app(db)
        .get("/api/posts")
        .send()
        .await
        .assert_ok()
        .assert_json_path("total", 2)
        .assert_json_count("data", 2)
        .assert_json_path("data.0.title", "First");

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn a_missing_row_is_a_404(db: Db) -> Result<()> {
    app(db)
        .get("/api/posts/999")
        .send()
        .await
        .assert_not_found()
        .assert_header("content-type", "application/problem+json; charset=utf-8")
        .assert_json_path("resource", "Post");

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn validation_failures_name_their_fields(db: Db) -> Result<()> {
    app(db)
        .post("/api/posts")
        .json(json!({ "title": "" }))
        .send()
        .await
        .assert_validation_errors(&["title"]);

    Ok(())
}

/// These two prove the rollback: each inserts a row, and each sees the same
/// starting count. If the transaction leaked, whichever ran second would fail.
#[luxid::test(db = crate::database)]
async fn writes_are_rolled_back_between_tests_a(db: Db) -> Result<()> {
    let app = app(db);

    app.get("/api/posts")
        .send()
        .await
        .assert_json_path("total", 2);
    app.post("/api/posts")
        .json(json!({ "title": "From A" }))
        .send()
        .await
        .assert_created();
    app.get("/api/posts")
        .send()
        .await
        .assert_json_path("total", 3);

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn writes_are_rolled_back_between_tests_b(db: Db) -> Result<()> {
    let app = app(db);

    app.get("/api/posts")
        .send()
        .await
        .assert_json_path("total", 2);
    app.post("/api/posts")
        .json(json!({ "title": "From B" }))
        .send()
        .await
        .assert_created();
    app.get("/api/posts")
        .send()
        .await
        .assert_json_path("total", 3);

    Ok(())
}

/// No database factory: `#[luxid::test]` is then `#[tokio::test]` with `Result`
/// unwrapping.
#[luxid::test]
async fn works_without_a_database() -> Result<()> {
    let app = TestApp::new(
        App::new()
            .routes(|r| {
                r.get("/ping", PingController::show);
            })
            .into_service(),
    );

    app.get("/ping")
        .send()
        .await
        .assert_ok()
        .assert_json_path("pong", true);

    Ok(())
}

pub struct PingController;

#[luxid::controller]
impl PingController {
    async fn show(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({ "pong": true }))
    }
}
