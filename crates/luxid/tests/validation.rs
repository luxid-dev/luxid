//! Form-request validation, including the database-backed rules.

use luxid::prelude::*;
use luxid_testing::TestApp;
use sea_orm::{ActiveValue::Set, ConnectionTrait};
use serde::Deserialize;
use serde_json::json;

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
        pub email: String,
        pub team_id: i64,

        #[sea_orm(ignore)]
        #[serde(flatten)]
        pub relations: luxid::Relations,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

use teams::Model as Team;
use users::Model as User;

fn not_reserved(name: &String) -> bool {
    name != "admin"
}

/// The form request. This is the shape §2 promised and controllers finally get.
#[derive(Debug, Deserialize, Validate)]
pub struct StoreUser {
    #[validate(length(min = 2, max = 64), custom(function = not_reserved, message = "is reserved"))]
    pub name: String,

    #[validate(email, unique(User::email))]
    pub email: String,

    #[validate(exists(Team::id))]
    pub team_id: i64,

    #[validate(range(min = 18, max = 120))]
    pub age: Option<i64>,
}

pub struct UsersController;

#[luxid::controller]
impl UsersController {
    async fn store(ctx: HttpContext) -> Result<Response> {
        let input: StoreUser = ctx.request.validate().await?;

        let created = luxid::insert(users::ActiveModel {
            name: Set(input.name),
            email: Set(input.email),
            team_id: Set(input.team_id),
            ..Default::default()
        })
        .await?;

        ctx.response.created(created)
    }
}

const SCHEMA: &str = "
    CREATE TABLE teams (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);
    CREATE TABLE users (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        email TEXT NOT NULL,
        team_id INTEGER NOT NULL
    );
";

pub async fn database() -> Db {
    let db = Db::in_memory().await.expect("opens");
    db.connection()
        .execute_unprepared(SCHEMA)
        .await
        .expect("creates schema");

    db.scope(async {
        luxid::insert(teams::ActiveModel {
            name: Set("Core".to_owned()),
            ..Default::default()
        })
        .await
        .expect("seeds");

        luxid::insert(users::ActiveModel {
            name: Set("Ada".to_owned()),
            email: Set("taken@example.com".to_owned()),
            team_id: Set(1),
            ..Default::default()
        })
        .await
        .expect("seeds");
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
                r.post("/users", UsersController::store);
            })
            .into_service(),
    )
}

#[luxid::test(db = crate::database)]
async fn a_valid_request_is_accepted(db: Db) -> Result<()> {
    app(db)
        .post("/users")
        .json(json!({ "name": "Grace", "email": "grace@example.com", "team_id": 1, "age": 45 }))
        .send()
        .await
        .assert_created()
        .assert_json_path("email", "grace@example.com");

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn every_failure_is_reported_in_one_response(db: Db) -> Result<()> {
    app(db)
        .post("/users")
        .json(json!({ "name": "G", "email": "not-an-email", "team_id": 999, "age": 5 }))
        .send()
        .await
        // Four independent problems, one round trip.
        .assert_validation_errors(&["name", "email", "team_id", "age"])
        .assert_validation_message("name", "must be at least 2 characters")
        .assert_validation_message("email", "must be a valid email address")
        .assert_validation_message("team_id", "does not exist")
        .assert_validation_message("age", "must be at least 18");

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn unique_consults_the_database(db: Db) -> Result<()> {
    app(db)
        .post("/users")
        .json(json!({ "name": "Grace", "email": "taken@example.com", "team_id": 1 }))
        .send()
        .await
        .assert_validation_errors(&["email"])
        .assert_validation_message("email", "has already been taken");

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn a_malformed_field_is_not_also_queried(db: Db) -> Result<()> {
    // `email` fails synchronously, so the `unique` rule is skipped: one
    // mistake, one message — not "invalid" *and* "already taken".
    app(db)
        .post("/users")
        .json(json!({ "name": "Grace", "email": "nonsense", "team_id": 1 }))
        .send()
        .await
        .assert_validation_errors(&["email"])
        .assert_json_count("errors.email", 1)
        .assert_validation_message("email", "must be a valid email address");

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn custom_rules_carry_their_message(db: Db) -> Result<()> {
    app(db)
        .post("/users")
        .json(json!({ "name": "admin", "email": "admin@example.com", "team_id": 1 }))
        .send()
        .await
        .assert_validation_errors(&["name"])
        .assert_validation_message("name", "is reserved");

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn absent_optional_fields_skip_their_rules(db: Db) -> Result<()> {
    // `age` is absent, so its range rule does not apply.
    app(db)
        .post("/users")
        .json(json!({ "name": "Grace", "email": "grace@example.com", "team_id": 1 }))
        .send()
        .await
        .assert_created();

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn a_malformed_body_is_a_400_not_a_422(db: Db) -> Result<()> {
    // A body that cannot be deserialized is a broken request, not a failed
    // validation — the client cannot fix it field by field.
    app(db)
        .post("/users")
        .json(json!({ "name": "Grace" }))
        .send()
        .await
        .assert_status(400);

    Ok(())
}
