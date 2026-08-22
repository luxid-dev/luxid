//! Scopes: reusable query fragments, usable as a starter or mid-chain.

use luxid::Query;
use luxid::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait};

mod users {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, serde::Serialize, luxid::Model)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
        pub team_id: i64,
        pub deleted_at: Option<String>,

        #[sea_orm(ignore)]
        #[serde(flatten)]
        pub relations: luxid::Relations,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

mod posts {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, serde::Serialize, luxid::Model)]
    #[sea_orm(table_name = "posts")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub title: String,
        pub user_id: i64,

        #[sea_orm(ignore)]
        #[serde(flatten)]
        pub relations: luxid::Relations,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

use posts::Model as Post;
use users::Model as User;

#[luxid::model(has_many(posts = Post, fk = "user_id"))]
impl User {
    /// Not soft-deleted.
    #[scope]
    fn active(query: Query<users::Entity>) -> Query<users::Entity> {
        query.where_null(User::deleted_at)
    }

    /// Scopes take arguments like any other function.
    #[scope]
    fn in_team(query: Query<users::Entity>, team_id: i64) -> Query<users::Entity> {
        query.where_eq(User::team_id, team_id)
    }

    #[scope]
    fn named_like(query: Query<users::Entity>, pattern: &str) -> Query<users::Entity> {
        query.where_like(User::name, pattern)
    }

    /// An ordinary associated function, untouched by the macro.
    pub fn describe() -> &'static str {
        "users"
    }
}

// `UserScopes` is generated alongside this impl block, so it is already in
// scope here. In an app where the model lives in its own module, mid-chain use
// needs `use crate::models::user::UserScopes;`.

#[luxid::model()]
impl Post {}

const SCHEMA: &str = "
    CREATE TABLE users (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        team_id INTEGER NOT NULL,
        deleted_at TEXT
    );
    CREATE TABLE posts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        title TEXT NOT NULL,
        user_id INTEGER NOT NULL
    );
";

async fn seeded() -> Db {
    let db = Db::in_memory().await.expect("opens");
    db.connection()
        .execute_unprepared(SCHEMA)
        .await
        .expect("creates schema");

    db.scope(async {
        for (name, team, deleted) in [
            ("Ada", 1, None),
            ("Alan", 1, None),
            ("Grace", 2, None),
            ("Removed", 1, Some("2026-01-01")),
        ] {
            let user = luxid::insert(users::ActiveModel {
                name: Set(name.to_owned()),
                team_id: Set(team),
                deleted_at: Set(deleted.map(str::to_owned)),
                ..Default::default()
            })
            .await
            .expect("inserts");

            luxid::insert(posts::ActiveModel {
                title: Set(format!("{name}'s post")),
                user_id: Set(user.id),
                ..Default::default()
            })
            .await
            .expect("inserts");
        }
    })
    .await;

    db
}

#[tokio::test]
async fn a_scope_works_as_a_starter_without_importing_anything() {
    let db = seeded().await;

    db.scope(async {
        // `User::active()` is an associated function — no trait import needed.
        let active = User::active().all().await.expect("queries");

        assert_eq!(active.len(), 3);
        assert!(active.iter().all(|user| user.deleted_at.is_none()));
    })
    .await;
}

#[tokio::test]
async fn a_scope_chains_mid_query() {
    let db = seeded().await;

    db.scope(async {
        let found = User::query()
            .where_eq(User::team_id, 1)
            .active()
            .order_by_asc(User::id)
            .all()
            .await
            .expect("queries");

        let names: Vec<_> = found.iter().map(|user| user.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Ada", "Alan"],
            "team 1, excluding the removed user"
        );
    })
    .await;
}

#[tokio::test]
async fn scopes_compose_with_each_other() {
    let db = seeded().await;

    db.scope(async {
        let count = User::active().in_team(1).count().await.expect("counts");
        assert_eq!(count, 2);

        let other = User::query()
            .in_team(2)
            .active()
            .count()
            .await
            .expect("counts");
        assert_eq!(other, 1);
    })
    .await;
}

#[tokio::test]
async fn scopes_take_arguments() {
    let db = seeded().await;

    db.scope(async {
        let found = User::named_like("A%")
            .order_by_asc(User::id)
            .all()
            .await
            .expect("queries");

        let names: Vec<_> = found.iter().map(|user| user.name.as_str()).collect();
        assert_eq!(names, vec!["Ada", "Alan"]);
    })
    .await;
}

#[tokio::test]
async fn scopes_compose_with_eager_loading_and_pagination() {
    let db = seeded().await;

    db.scope(async {
        let page = User::active()
            .in_team(1)
            .with("posts")
            .order_by_asc(User::id)
            .paginate(1, 1)
            .await
            .expect("paginates");

        assert_eq!(page.total, 2);
        assert_eq!(page.data.len(), 1);
        assert_eq!(page.data[0].name, "Ada");
        assert_eq!(page.data[0].posts().expect("loaded").len(), 1);
    })
    .await;
}

#[test]
fn ordinary_associated_functions_survive_the_macro() {
    assert_eq!(User::describe(), "users");
}
