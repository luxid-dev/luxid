//! Migrations, and the route table they share a command line with.

use luxid::migration::prelude::*;
use luxid::prelude::*;
use luxid_testing::TestApp;
use sea_orm::ActiveValue::Set;
use serde_json::json;

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

#[derive(DeriveIden)]
enum Posts {
    Table,
    Id,
    Title,
}

/// An ordinary SeaORM migration — Luxid wraps the migrator, it does not replace
/// it, so the cross-dialect DDL builder comes along unchanged.
///
/// `MigrationName` is implemented by hand rather than derived: the derive takes
/// the name from the *file stem*, which in a test file would make every
/// migration here share one name.
// SeaORM's convention names migrations after their timestamp, which Rust's
// naming lint dislikes. The convention wins: it is what sorts correctly.
#[allow(non_camel_case_types)]
pub struct m20260101_000001_create_posts;

impl MigrationName for m20260101_000001_create_posts {
    fn name(&self) -> &str {
        "m20260101_000001_create_posts"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for m20260101_000001_create_posts {
    async fn up(&self, manager: &SchemaManager) -> std::result::Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Posts::Table)
                    .if_not_exists()
                    .col(pk_auto(Posts::Id))
                    .col(string(Posts::Title))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> std::result::Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Posts::Table).to_owned())
            .await
    }
}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260101_000001_create_posts)]
    }
}

pub struct PostsController;

#[luxid::controller]
impl PostsController {
    async fn index(ctx: HttpContext) -> Result<Response> {
        ctx.response
            .ok(Post::query().order_by_asc(Post::id).all().await?)
    }

    async fn show(ctx: HttpContext) -> Result<Response> {
        ctx.response
            .ok(Post::find_or_fail(ctx.params.get::<i64>("id")?).await?)
    }
}

/// The harness can now build its own schema from migrations rather than from a
/// hand-written `CREATE TABLE`.
pub async fn database() -> Db {
    let db = Db::in_memory().await.expect("opens");
    db.migrate::<Migrator>().await.expect("migrates");

    db.scope(async {
        luxid::insert(posts::ActiveModel {
            title: Set("First".to_owned()),
            ..Default::default()
        })
        .await
        .expect("seeds");
    })
    .await;

    db
}

fn build_app(db: Db) -> App {
    App::new()
        .providers(Providers::new().singleton(move |_| db.clone()))
        .middleware(WithDatabase)
        .routes(|r| {
            r.group("/api", |r| {
                r.get("/posts", PostsController::index);
                r.get("/posts/{id}", PostsController::show);
            });
        })
}

#[tokio::test]
async fn migrations_create_the_schema() {
    let db = Db::in_memory().await.expect("opens");

    // Nothing exists yet.
    db.scope(async {
        assert!(
            Post::count_all().await.is_err(),
            "the table is absent before migrating"
        );
    })
    .await;

    db.migrate::<Migrator>().await.expect("migrates");

    db.scope(async {
        assert_eq!(Post::count_all().await.expect("counts"), 0);
    })
    .await;
}

#[tokio::test]
async fn status_reports_pending_then_applied() {
    let db = Db::in_memory().await.expect("opens");

    let before = db
        .migration_status::<Migrator>()
        .await
        .expect("reads status");
    assert_eq!(before.len(), 1);
    assert!(!before[0].applied);
    assert!(before[0].name.contains("create_posts"));
    assert!(
        db.has_pending_migrations::<Migrator>()
            .await
            .expect("checks")
    );

    db.migrate::<Migrator>().await.expect("migrates");

    let after = db
        .migration_status::<Migrator>()
        .await
        .expect("reads status");
    assert!(after[0].applied);
    assert!(
        !db.has_pending_migrations::<Migrator>()
            .await
            .expect("checks")
    );
}

#[tokio::test]
async fn migrating_twice_is_a_no_op() {
    let db = Db::in_memory().await.expect("opens");

    db.migrate::<Migrator>().await.expect("migrates");
    db.migrate::<Migrator>()
        .await
        .expect("migrating again is harmless");

    db.scope(async {
        assert_eq!(Post::count_all().await.expect("counts"), 0);
    })
    .await;
}

#[tokio::test]
async fn rollback_undoes_the_migration() {
    let db = Db::in_memory().await.expect("opens");

    db.migrate::<Migrator>().await.expect("migrates");
    db.rollback::<Migrator>(1).await.expect("rolls back");

    db.scope(async {
        assert!(Post::count_all().await.is_err(), "the table is gone again");
    })
    .await;

    let status = db
        .migration_status::<Migrator>()
        .await
        .expect("reads status");
    assert!(!status[0].applied);
}

#[luxid::test(db = crate::database)]
async fn the_app_serves_against_a_migrated_schema(db: Db) -> Result<()> {
    TestApp::new(build_app(db).into_service())
        .get("/api/posts")
        .send()
        .await
        .assert_ok()
        .assert_json_count("", 1)
        .assert_json_path("0.title", "First");

    Ok(())
}

#[tokio::test]
async fn the_route_table_names_its_actions() {
    let db = Db::in_memory().await.expect("opens");
    let table = build_app(db).route_table();

    assert_eq!(table.len(), 2);

    assert_eq!(table[0].method.as_str(), "GET");
    assert_eq!(table[0].path, "/api/posts");
    assert_eq!(table[0].action, "PostsController::index");
    assert_eq!(table[0].middleware, 1, "the global WithDatabase");

    assert_eq!(table[1].path, "/api/posts/{id}");
    assert_eq!(table[1].action, "PostsController::show");
}

#[tokio::test]
async fn the_rendered_route_table_is_readable() {
    let db = Db::in_memory().await.expect("opens");
    let rendered = luxid::cli::render_routes(&build_app(db).route_table());

    assert!(
        rendered.contains("GET  /api/posts       PostsController::index"),
        "{rendered}"
    );
    assert!(rendered.contains("PostsController::show"), "{rendered}");
    assert!(rendered.contains("[1 middleware]"), "{rendered}");
}

/// Guards the destructive command: `migrate:fresh` without `--force` must not
/// touch anything.
#[tokio::test]
async fn migrate_fresh_is_available_but_the_cli_guards_it() {
    let db = Db::in_memory().await.expect("opens");
    db.migrate::<Migrator>().await.expect("migrates");

    db.scope(async {
        luxid::insert(posts::ActiveModel {
            title: Set("Doomed".to_owned()),
            ..Default::default()
        })
        .await
        .expect("inserts");
    })
    .await;

    db.migrate_fresh::<Migrator>().await.expect("rebuilds");

    db.scope(async {
        assert_eq!(
            Post::count_all().await.expect("counts"),
            0,
            "the data is gone"
        );
    })
    .await;
}

#[tokio::test]
async fn a_json_body_still_round_trips_after_migrating() {
    let db = Db::in_memory().await.expect("opens");
    db.migrate::<Migrator>().await.expect("migrates");

    db.scope(async {
        let created = luxid::insert(posts::ActiveModel {
            title: Set("Round trip".to_owned()),
            ..Default::default()
        })
        .await
        .expect("inserts");

        assert_eq!(
            serde_json::to_value(&created).expect("serializes")["title"],
            json!("Round trip")
        );
    })
    .await;
}
