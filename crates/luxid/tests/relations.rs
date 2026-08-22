//! Relations: batched eager loading, typed accessors, strict-mode N+1
//! detection, and inline serialization.

use luxid::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait};
use serde_json::Value;

mod teams {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, serde::Serialize, luxid::Model)]
    #[sea_orm(table_name = "teams")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,

        #[sea_orm(ignore)]
        #[serde(flatten)]
        pub relations: luxid::Relations,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

mod users {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, serde::Serialize, luxid::Model)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
        pub team_id: i64,

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
use teams::Model as Team;
use users::Model as User;

#[luxid::model(
    has_many(posts = Post, fk = "user_id"),
    belongs_to(team = Team),
)]
impl User {}

#[luxid::model(has_many(members = User, fk = "team_id"))]
impl Team {}

#[luxid::model(belongs_to(author = User, fk = "user_id", local_key = "id"))]
impl Post {}

const SCHEMA: &str = "
    CREATE TABLE teams (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);
    CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, team_id INTEGER NOT NULL);
    CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, user_id INTEGER NOT NULL);
";

async fn seeded() -> Db {
    let db = Db::in_memory().await.expect("opens");
    db.connection()
        .execute_unprepared(SCHEMA)
        .await
        .expect("creates schema");

    db.scope(async {
        let engineering = luxid::insert(teams::ActiveModel {
            name: Set("Engineering".to_owned()),
            ..Default::default()
        })
        .await
        .expect("inserts");

        let design = luxid::insert(teams::ActiveModel {
            name: Set("Design".to_owned()),
            ..Default::default()
        })
        .await
        .expect("inserts");

        for (name, team) in [
            ("Ada", engineering.id),
            ("Alan", engineering.id),
            ("Grace", design.id),
        ] {
            let user = luxid::insert(users::ActiveModel {
                name: Set(name.to_owned()),
                team_id: Set(team),
                ..Default::default()
            })
            .await
            .expect("inserts");

            // Ada gets two posts, Alan one, Grace none.
            let count = match name {
                "Ada" => 2,
                "Alan" => 1,
                _ => 0,
            };

            for n in 0..count {
                luxid::insert(posts::ActiveModel {
                    title: Set(format!("{name} post {n}")),
                    user_id: Set(user.id),
                    ..Default::default()
                })
                .await
                .expect("inserts");
            }
        }
    })
    .await;

    db
}

#[tokio::test]
async fn has_many_loads_for_every_parent_including_empty_ones() {
    let db = seeded().await;

    db.scope(async {
        let users = User::query()
            .with("posts")
            .order_by_asc(User::id)
            .all()
            .await
            .expect("queries");

        assert_eq!(users.len(), 3);
        assert_eq!(users[0].posts().expect("loaded").len(), 2, "Ada");
        assert_eq!(users[1].posts().expect("loaded").len(), 1, "Alan");

        // A parent with no children is *loaded and empty*, not unloaded —
        // otherwise strict mode would fire on a correct query.
        assert!(users[2].posts().expect("loaded").is_empty(), "Grace");
    })
    .await;
}

#[tokio::test]
async fn belongs_to_resolves_the_owner() {
    let db = seeded().await;

    db.scope(async {
        let users = User::query()
            .with("team")
            .order_by_asc(User::id)
            .all()
            .await
            .expect("queries");

        assert_eq!(
            users[0].team().expect("loaded").expect("has a team").name,
            "Engineering"
        );
        assert_eq!(
            users[2].team().expect("loaded").expect("has a team").name,
            "Design"
        );
    })
    .await;
}

#[tokio::test]
async fn several_relations_load_together() {
    let db = seeded().await;

    db.scope(async {
        let user = User::query()
            .with("posts")
            .with("team")
            .order_by_asc(User::id)
            .first()
            .await
            .expect("queries")
            .expect("exists");

        assert_eq!(user.posts().expect("loaded").len(), 2);
        assert_eq!(
            user.team().expect("loaded").expect("has a team").name,
            "Engineering"
        );
    })
    .await;
}

#[tokio::test]
async fn relations_load_on_a_paginated_page() {
    let db = seeded().await;

    db.scope(async {
        let page = User::query()
            .with("posts")
            .order_by_asc(User::id)
            .paginate(1, 2)
            .await
            .expect("paginates");

        assert_eq!(page.data.len(), 2);
        assert_eq!(page.data[0].posts().expect("loaded").len(), 2);
        assert_eq!(page.data[1].posts().expect("loaded").len(), 1);
    })
    .await;
}

#[tokio::test]
async fn reading_an_unloaded_relation_is_an_n_plus_one_failure() {
    let db = seeded().await;
    luxid::set_strict_relations(true);

    db.scope(async {
        let users = User::query().all().await.expect("queries");
        let err = users[0].posts().unwrap_err();

        let message = format!("{err}");
        assert!(message.contains("was not loaded"), "{message}");
        assert!(message.contains(".with(\"posts\")"), "{message}");
    })
    .await;
}

#[tokio::test]
async fn an_undeclared_relation_lists_what_is_available() {
    let db = seeded().await;

    db.scope(async {
        let err = User::query().with("commnets").all().await.unwrap_err();

        let message = format!("{err}");
        assert!(message.contains("has no relation `commnets`"), "{message}");
        assert!(message.contains("posts"), "{message}");
        assert!(message.contains("team"), "{message}");
    })
    .await;
}

#[tokio::test]
async fn loaded_relations_serialize_inline_with_the_model() {
    let db = seeded().await;

    db.scope(async {
        let user = User::query()
            .with("posts")
            .with("team")
            .order_by_asc(User::id)
            .first()
            .await
            .expect("queries")
            .expect("exists");

        let json: Value = serde_json::to_value(&user).expect("serializes");

        assert_eq!(json["name"], "Ada");
        assert_eq!(json["posts"].as_array().expect("array").len(), 2);
        assert_eq!(json["team"]["name"], "Engineering");
    })
    .await;
}

#[tokio::test]
async fn an_unloaded_model_serializes_without_relation_keys() {
    let db = seeded().await;

    db.scope(async {
        let user = User::query()
            .order_by_asc(User::id)
            .first()
            .await
            .expect("queries")
            .expect("exists");
        let json: Value = serde_json::to_value(&user).expect("serializes");

        assert_eq!(json["name"], "Ada");
        assert!(
            json.get("posts").is_none(),
            "nothing loaded, nothing rendered"
        );
    })
    .await;
}

#[tokio::test]
async fn the_inverse_direction_also_works() {
    let db = seeded().await;

    db.scope(async {
        let posts = Post::query()
            .with("author")
            .order_by_asc(Post::id)
            .all()
            .await
            .expect("queries");

        assert_eq!(
            posts[0]
                .author()
                .expect("loaded")
                .expect("has an author")
                .name,
            "Ada"
        );

        let teams = Team::query()
            .with("members")
            .order_by_asc(Team::id)
            .all()
            .await
            .expect("queries");

        assert_eq!(teams[0].members().expect("loaded").len(), 2);
        assert_eq!(teams[1].members().expect("loaded").len(), 1);
    })
    .await;
}

#[tokio::test]
async fn eager_loading_an_empty_result_set_is_not_an_error() {
    let db = seeded().await;

    db.scope(async {
        let none = User::query()
            .where_eq(User::name, "Nobody")
            .with("posts")
            .all()
            .await
            .expect("queries");

        assert!(none.is_empty());
    })
    .await;
}
