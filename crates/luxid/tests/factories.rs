//! Factories and the `acting_as` shortcut.

use luxid::prelude::*;
use luxid_testing::TestApp;
use sea_orm::{ActiveValue::Set, ConnectionTrait};
use serde_json::json;

mod users {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, serde::Serialize, luxid::Model)]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
        pub email: String,
        pub role: String,

        #[sea_orm(ignore)]
        #[serde(flatten)]
        pub relations: luxid::Relations,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

use users::Model as User;

/// Counter making each generated row distinct, the way a real factory's fake
/// data would.
static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn next() -> u32 {
    NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

pub struct UserFactory;

impl Factory for UserFactory {
    type Active = users::ActiveModel;

    fn definition() -> Self::Active {
        let n = next();

        users::ActiveModel {
            name: Set(format!("User {n}")),
            email: Set(format!("user{n}@example.com")),
            role: Set("member".to_owned()),
            ..Default::default()
        }
    }
}

const SCHEMA: &str = "
    CREATE TABLE users (
        id    INTEGER PRIMARY KEY AUTOINCREMENT,
        name  TEXT NOT NULL,
        email TEXT NOT NULL,
        role  TEXT NOT NULL
    );
";

const SECRET: &str = "factory-test-secret";

pub async fn database() -> Db {
    let db = Db::in_memory().await.expect("opens");
    db.connection()
        .execute_unprepared(SCHEMA)
        .await
        .expect("creates schema");
    db
}

pub struct MeController;

#[luxid::controller]
impl MeController {
    async fn show(ctx: HttpContext) -> Result<Response> {
        let id: i64 = ctx.auth.id()?;
        let role: Option<String> = ctx.auth.identity()?.claim("role")?;

        ctx.response.ok(json!({ "id": id, "role": role }))
    }
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
            .routes(|r| {
                r.get("/me", MeController::show).middleware(Auth::jwt());
            })
            .into_service(),
    )
}

#[luxid::test(db = crate::database)]
async fn one_typical_row(db: Db) -> Result<()> {
    let _ = &db;

    let user = UserFactory::new().create_one().await?;

    assert!(user.name.starts_with("User "));
    assert!(user.email.ends_with("@example.com"));
    assert_eq!(user.role, "member");
    assert_eq!(User::count_all().await?, 1);

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn many_rows_are_distinct(db: Db) -> Result<()> {
    let _ = &db;

    let users = UserFactory::new().count(3).create().await?;

    assert_eq!(users.len(), 3);
    assert_eq!(User::count_all().await?, 3);

    // A factory that produced three identical rows would break any test
    // asserting on a unique column.
    let mut emails: Vec<_> = users.iter().map(|user| user.email.clone()).collect();
    emails.sort();
    emails.dedup();
    assert_eq!(emails.len(), 3);

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn a_state_overrides_only_what_it_names(db: Db) -> Result<()> {
    let _ = &db;

    let user = UserFactory::new()
        .state(|row| row.role = Set("admin".to_owned()))
        .create_one()
        .await?;

    assert_eq!(user.role, "admin", "the override applied");
    assert!(
        user.name.starts_with("User "),
        "the rest of the definition survived"
    );

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn later_states_win(db: Db) -> Result<()> {
    let _ = &db;

    let user = UserFactory::new()
        .state(|row| row.role = Set("member".to_owned()))
        .state(|row| row.role = Set("owner".to_owned()))
        .create_one()
        .await?;

    assert_eq!(user.role, "owner");
    Ok(())
}

#[luxid::test(db = crate::database)]
async fn create_one_ignores_count(db: Db) -> Result<()> {
    let _ = &db;

    UserFactory::new().count(5).create_one().await?;

    assert_eq!(User::count_all().await?, 1, "create_one means one");
    Ok(())
}

#[tokio::test]
async fn make_builds_rows_without_touching_the_database() {
    // No database scope at all — `make` must not need one.
    let rows = UserFactory::new().count(2).make();

    assert_eq!(rows.len(), 2);
    assert!(matches!(rows[0].role, sea_orm::ActiveValue::Set(_)));
}

#[luxid::test(db = crate::database)]
async fn acting_as_reaches_the_action_through_the_real_guard(db: Db) -> Result<()> {
    let user = UserFactory::new().create_one().await?;

    app(db)
        .get("/me")
        .acting_as(SECRET, user.id)
        .send()
        .await
        .assert_ok()
        .assert_json_path("id", user.id);

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn acting_as_can_carry_claims(db: Db) -> Result<()> {
    let user = UserFactory::new().create_one().await?;

    app(db)
        .get("/me")
        .acting_as_with(SECRET, user.id, [("role".to_owned(), json!("admin"))])
        .send()
        .await
        .assert_ok()
        .assert_json_path("role", "admin");

    Ok(())
}

#[luxid::test(db = crate::database)]
async fn without_acting_as_the_guard_still_rejects(db: Db) -> Result<()> {
    // Proof that `acting_as` goes through the guard rather than around it.
    app(db).get("/me").send().await.assert_unauthorized();
    Ok(())
}
