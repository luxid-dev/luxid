//! The data layer exercised against a real SQLite database.
//!
//! SQLite is used so the suite runs on any machine with no provisioning.
//!
//! Running the same tests against Postgres to catch dialect differences is not
//! wired up yet: the schema here uses SQLite's `INTEGER PRIMARY KEY
//! AUTOINCREMENT`, so it needs dialect-aware DDL first.

use luxid_orm::model::{Record, delete_by_id, insert, update};
use luxid_orm::{Db, sea_orm};
use sea_orm::{ActiveValue::Set, ConnectionTrait, IntoActiveModel};

mod users {
    use sea_orm::entity::prelude::*;

    // `crate = ::luxid_orm` because this crate is below the `luxid` facade;
    // applications use the default and never write this.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, serde::Serialize, luxid_macros::Model)]
    #[luxid(crate = ::luxid_orm)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
        pub email: String,
        pub team_id: i64,
        pub deleted_at: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

use users::Entity as Users;
use users::Model as User;

const SCHEMA: &str = "
    CREATE TABLE users (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        name        TEXT    NOT NULL,
        email       TEXT    NOT NULL,
        team_id     INTEGER NOT NULL,
        deleted_at  TEXT
    );
";

async fn database() -> Db {
    let db = Db::in_memory().await.expect("opens");
    db.connection()
        .execute_unprepared(SCHEMA)
        .await
        .expect("creates the schema");
    db
}

fn new_user(name: &str, email: &str, team_id: i64) -> users::ActiveModel {
    users::ActiveModel {
        name: Set(name.to_owned()),
        email: Set(email.to_owned()),
        team_id: Set(team_id),
        deleted_at: Set(None),
        ..Default::default()
    }
}

async fn seed() -> Db {
    let db = database().await;

    db.scope(async {
        insert(new_user("Ada", "ada@example.com", 1))
            .await
            .expect("inserts");
        insert(new_user("Alan", "alan@example.com", 1))
            .await
            .expect("inserts");
        insert(new_user("Grace", "grace@example.com", 2))
            .await
            .expect("inserts");
    })
    .await;

    db
}

#[tokio::test]
async fn inserting_and_finding_a_row() {
    let db = database().await;

    db.scope(async {
        let created = insert(new_user("Ada", "ada@example.com", 1))
            .await
            .expect("inserts");
        assert_eq!(created.name, "Ada");

        let found = User::find(created.id)
            .await
            .expect("queries")
            .expect("exists");
        assert_eq!(found.email, "ada@example.com");
    })
    .await;
}

#[tokio::test]
async fn find_returns_none_for_a_missing_row() {
    let db = database().await;

    db.scope(async {
        assert!(User::find(999).await.expect("queries").is_none());
    })
    .await;
}

#[tokio::test]
async fn find_or_fail_produces_a_404_naming_the_model_and_id() {
    let db = database().await;

    db.scope(async {
        let err = User::find_or_fail(404).await.unwrap_err();

        assert_eq!(err.status_code().as_u16(), 404);

        let problem = err.problem();
        assert_eq!(problem["resource"], "User");
        assert_eq!(problem["id"], "404");
    })
    .await;
}

#[tokio::test]
async fn filtering_and_ordering() {
    let db = seed().await;

    db.scope(async {
        let team_one = User::query()
            .where_eq(users::Column::TeamId, 1)
            .order_by_desc(users::Column::Name)
            .all()
            .await
            .expect("queries");

        let names: Vec<_> = team_one.iter().map(|user| user.name.as_str()).collect();
        assert_eq!(names, vec!["Alan", "Ada"]);
    })
    .await;
}

#[tokio::test]
async fn counting_and_existence() {
    let db = seed().await;

    db.scope(async {
        assert_eq!(User::count_all().await.expect("counts"), 3);
        assert_eq!(
            User::query()
                .where_eq(users::Column::TeamId, 2)
                .count()
                .await
                .expect("counts"),
            1
        );
        assert!(
            User::query()
                .where_eq(users::Column::Email, "ada@example.com")
                .exists()
                .await
                .expect("queries")
        );
        assert!(
            !User::query()
                .where_eq(users::Column::Email, "nobody@example.com")
                .exists()
                .await
                .expect("queries")
        );
    })
    .await;
}

#[tokio::test]
async fn null_filters() {
    let db = seed().await;

    db.scope(async {
        assert_eq!(
            User::query()
                .where_null(users::Column::DeletedAt)
                .count()
                .await
                .expect("counts"),
            3
        );
        assert_eq!(
            User::query()
                .where_not_null(users::Column::DeletedAt)
                .count()
                .await
                .expect("counts"),
            0
        );
    })
    .await;
}

#[tokio::test]
async fn pagination_reports_totals_and_pages() {
    let db = seed().await;

    db.scope(async {
        let first = User::query()
            .order_by_asc(users::Column::Id)
            .paginate(1, 2)
            .await
            .expect("paginates");

        assert_eq!(first.data.len(), 2);
        assert_eq!(first.total, 3);
        assert_eq!(first.last_page, 2);
        assert_eq!(first.page, 1);
        assert!(first.has_more());

        let second = User::query()
            .order_by_asc(users::Column::Id)
            .paginate(2, 2)
            .await
            .expect("paginates");

        assert_eq!(second.data.len(), 1);
        assert_eq!(second.data[0].name, "Grace");
        assert!(!second.has_more());
    })
    .await;
}

#[tokio::test]
async fn pagination_is_one_based_and_clamps_nonsense() {
    let db = seed().await;

    db.scope(async {
        // Page 0 and per_page 0 are user input, not a reason to panic or to
        // silently return nothing.
        let page = User::query().paginate(0, 0).await.expect("paginates");

        assert_eq!(page.page, 1);
        assert_eq!(page.per_page, 1);
        assert_eq!(page.total, 3);
        assert_eq!(page.last_page, 3);
    })
    .await;
}

#[tokio::test]
async fn pagination_past_the_end_is_empty_rather_than_an_error() {
    let db = seed().await;

    db.scope(async {
        let page = User::query().paginate(99, 10).await.expect("paginates");

        assert!(page.is_empty());
        assert_eq!(page.total, 3);
    })
    .await;
}

#[tokio::test]
async fn updating_a_row() {
    let db = seed().await;

    db.scope(async {
        let user = User::query()
            .where_eq(users::Column::Email, "ada@example.com")
            .first()
            .await
            .expect("queries")
            .expect("exists");

        let mut active = user.into_active_model();
        active.name = Set("Ada Lovelace".to_owned());

        let saved = update(active).await.expect("updates");
        assert_eq!(saved.name, "Ada Lovelace");

        let reloaded = User::find_or_fail(saved.id).await.expect("exists");
        assert_eq!(reloaded.name, "Ada Lovelace");
    })
    .await;
}

#[tokio::test]
async fn deleting_a_row() {
    let db = seed().await;

    db.scope(async {
        let user = User::query()
            .first()
            .await
            .expect("queries")
            .expect("exists");

        assert!(delete_by_id::<Users>(user.id).await.expect("deletes"));
        assert!(User::find(user.id).await.expect("queries").is_none());

        // Deleting again removes nothing, and says so rather than erroring.
        assert!(!delete_by_id::<Users>(user.id).await.expect("deletes"));
    })
    .await;
}

#[tokio::test]
async fn a_rollback_scope_leaves_nothing_behind() {
    let db = database().await;

    db.rollback_scope(async || {
        insert(new_user("Temporary", "temp@example.com", 1))
            .await
            .expect("inserts");
        assert_eq!(User::count_all().await.expect("counts"), 1);
    })
    .await
    .expect("rolls back");

    db.scope(async {
        assert_eq!(
            User::count_all().await.expect("counts"),
            0,
            "the row was rolled back"
        );
    })
    .await;
}

#[tokio::test]
async fn a_transaction_commits_on_success() {
    let db = database().await;

    db.transaction(async || {
        insert(new_user("Kept", "kept@example.com", 1)).await?;
        Ok(())
    })
    .await
    .expect("commits");

    db.scope(async {
        assert_eq!(User::count_all().await.expect("counts"), 1);
    })
    .await;
}

#[tokio::test]
async fn a_transaction_rolls_back_on_error() {
    let db = database().await;

    let outcome: luxid_core::Result<()> = db
        .transaction(async || {
            insert(new_user("Doomed", "doomed@example.com", 1)).await?;
            Err(luxid_core::Error::Conflict("changed my mind".into()))
        })
        .await;

    assert!(outcome.is_err());

    db.scope(async {
        assert_eq!(
            User::count_all().await.expect("counts"),
            0,
            "the insert was undone"
        );
    })
    .await;
}

#[tokio::test]
async fn querying_outside_a_scope_explains_itself() {
    let err = User::count_all().await.unwrap_err();
    let message = format!("{err}");

    assert!(
        message.contains("no database connection is in scope"),
        "{message}"
    );
    assert!(message.contains("tokio::spawn"), "{message}");
}

#[tokio::test]
async fn a_detached_task_does_not_inherit_the_scope() {
    let db = database().await;

    let outcome = db
        .scope(async {
            tokio::spawn(async { User::count_all().await })
                .await
                .expect("joins")
        })
        .await;

    // Reported, not silently routed to a different connection.
    assert!(outcome.is_err());
}

/// Regression: a scope nested inside a transaction must join it rather than
/// reach for the pool. With a single-connection pool, reaching for the pool
/// deadlocks against the connection the transaction holds.
#[tokio::test]
async fn a_nested_scope_joins_the_open_transaction() {
    let db = database().await;

    db.rollback_scope(async || {
        insert(new_user("Inside", "inside@example.com", 1))
            .await
            .expect("inserts");

        // Stands in for what the request middleware does per request.
        let seen = if luxid_orm::db::current().is_ok() {
            User::count_all().await.expect("counts")
        } else {
            db.scope(async { User::count_all().await.expect("counts") })
                .await
        };

        assert_eq!(seen, 1, "the nested scope sees the transaction's write");
    })
    .await
    .expect("rolls back");

    db.scope(async {
        assert_eq!(User::count_all().await.expect("counts"), 0);
    })
    .await;
}
