//! The whole stack together: HTTP → controller → data layer → SQLite → JSON.

use luxid::__private::salvo::test::{ResponseExt, TestClient};
use luxid::prelude::*;
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
        pub published: i32,
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
        let page = ctx.request.input::<u64>("page")?.unwrap_or(1);

        let posts = Post::query()
            .where_eq(Post::published, 1)
            .order_by_asc(Post::id)
            .paginate(page, 2)
            .await?;

        ctx.response.ok(posts)
    }

    async fn show(ctx: HttpContext) -> Result<Response> {
        let id: i64 = ctx.params.get("id")?;

        // The line that keeps actions free of error handling: a missing row
        // becomes a 404 with no branching here.
        ctx.response.ok(Post::find_or_fail(id).await?)
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
            published: Set(1),
            ..Default::default()
        })
        .await?;

        ctx.response.created(created)
    }
}

const SCHEMA: &str = "
    CREATE TABLE posts (
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        title     TEXT    NOT NULL,
        published INTEGER NOT NULL
    );
";

async fn service() -> luxid::__private::salvo::Service {
    let db = Db::in_memory().await.expect("opens");
    db.connection()
        .execute_unprepared(SCHEMA)
        .await
        .expect("creates schema");

    db.scope(async {
        for (title, published) in [("First", 1), ("Second", 1), ("Third", 1), ("Draft", 0)] {
            luxid::insert(posts::ActiveModel {
                title: Set(title.to_owned()),
                published: Set(published),
                ..Default::default()
            })
            .await
            .expect("seeds");
        }
    })
    .await;

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
        .into_service()
}

/// As [`service`], but every request is rolled back afterwards.
async fn rollback_service() -> luxid::__private::salvo::Service {
    let db = Db::in_memory().await.expect("opens");
    db.connection()
        .execute_unprepared(SCHEMA)
        .await
        .expect("creates schema");

    App::new()
        .providers(Providers::new().singleton(move |_| db.clone()))
        .middleware(WithRollbackDatabase)
        .routes(|r| {
            r.group("/api", |r| {
                r.get("/posts", PostsController::index);
                r.post("/posts", PostsController::store);
            });
        })
        .into_service()
}

const BASE: &str = "http://127.0.0.1:5800/api";

#[tokio::test]
async fn a_paginated_index_serializes_in_the_laravel_shape() {
    let mut res = TestClient::get(format!("{BASE}/posts"))
        .send(&service().await)
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(200));

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["total"], 3, "the unpublished draft is filtered out");
    assert_eq!(body["page"], 1);
    assert_eq!(body["per_page"], 2);
    assert_eq!(body["last_page"], 2);
    assert_eq!(body["data"][0]["title"], "First");
    assert_eq!(body["data"].as_array().expect("array").len(), 2);
}

#[tokio::test]
async fn the_page_query_parameter_drives_pagination() {
    let mut res = TestClient::get(format!("{BASE}/posts?page=2"))
        .send(&service().await)
        .await;

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["page"], 2);
    assert_eq!(body["data"][0]["title"], "Third");
}

#[tokio::test]
async fn a_row_is_returned_as_json() {
    let mut res = TestClient::get(format!("{BASE}/posts/1"))
        .send(&service().await)
        .await;

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["id"], 1);
    assert_eq!(body["title"], "First");
}

#[tokio::test]
async fn a_missing_row_becomes_a_404_problem_document() {
    let mut res = TestClient::get(format!("{BASE}/posts/999"))
        .send(&service().await)
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(404));
    assert_eq!(
        res.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json; charset=utf-8")
    );

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["resource"], "Post");
    assert_eq!(body["id"], "999");
}

#[tokio::test]
async fn writes_persist_and_are_readable() {
    let service = service().await;

    let mut created = TestClient::post(format!("{BASE}/posts"))
        .json(&json!({ "title": "Fourth" }))
        .send(&service)
        .await;

    assert_eq!(created.status_code.map(|s| s.as_u16()), Some(201));

    let body: Value = created.take_json().await.expect("json body");
    let id = body["id"].as_i64().expect("an id");

    let mut fetched = TestClient::get(format!("{BASE}/posts/{id}"))
        .send(&service)
        .await;
    let fetched: Value = fetched.take_json().await.expect("json body");

    assert_eq!(fetched["title"], "Fourth");
}

#[tokio::test]
async fn validation_still_short_circuits_before_touching_the_database() {
    let service = service().await;

    let mut res = TestClient::post(format!("{BASE}/posts"))
        .json(&json!({ "title": "" }))
        .send(&service)
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(422));

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["errors"]["title"][0], "is required");

    // Nothing was written.
    let mut index = TestClient::get(format!("{BASE}/posts"))
        .send(&service)
        .await;
    let index: Value = index.take_json().await.expect("json body");
    assert_eq!(index["total"], 3);
}

#[tokio::test]
async fn a_route_without_the_database_middleware_reports_the_missing_scope() {
    let service = App::new()
        .routes(|r| {
            r.get("/posts/{id}", PostsController::show);
        })
        .into_service();

    let mut res = TestClient::get("http://127.0.0.1:5800/posts/1")
        .send(&service)
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(500));

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["title"], "internal server error");
}

#[tokio::test]
async fn the_rollback_middleware_discards_each_request_s_writes() {
    let service = rollback_service().await;

    let created = TestClient::post(format!("{BASE}/posts"))
        .json(&json!({ "title": "Ephemeral" }))
        .send(&service)
        .await;

    assert_eq!(created.status_code.map(|s| s.as_u16()), Some(201));

    // The next request cannot see it: the transaction was rolled back.
    let mut index = TestClient::get(format!("{BASE}/posts"))
        .send(&service)
        .await;
    let index: Value = index.take_json().await.expect("json body");

    assert_eq!(index["total"], 0);
}
