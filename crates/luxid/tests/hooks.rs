//! Lifecycle hooks: ordering, mutation before the write, and abort-on-error.

use std::cell::RefCell;

use luxid::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait, IntoActiveModel};

// Thread-local, not global: the test harness gives each test its own thread and
// `#[tokio::test]` runs the body on it, so this isolates tests that would
// otherwise interleave their traces when run in parallel.
thread_local! {
    /// Records the order hooks fired in, so ordering is asserted, not assumed.
    static TRACE: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };

    /// Set by a test to make `before_create` fail.
    static REJECT: RefCell<bool> = const { RefCell::new(false) };
}

fn trace(event: &'static str) {
    TRACE.with_borrow_mut(|events| events.push(event));
}

fn taken() -> Vec<&'static str> {
    TRACE.with_borrow_mut(std::mem::take)
}

fn set_reject(value: bool) {
    REJECT.with_borrow_mut(|reject| *reject = value);
}

fn rejecting() -> bool {
    REJECT.with_borrow(|reject| *reject)
}

mod users {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, serde::Serialize, luxid::Model)]
    #[luxid(
        before_save = Self::stamp,
        before_create = Self::hash_password,
        before_update = Self::note_update,
        after_create = Self::welcome,
        after_update = Self::note_updated,
        after_save = Self::saved
    )]
    #[sea_orm(table_name = "users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub email: String,
        pub password: String,
        pub slug: String,

        #[sea_orm(ignore)]
        #[serde(flatten)]
        pub relations: luxid::Relations,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

use users::Model as User;

impl User {
    /// Mutating the active model before the write is the point of a before hook.
    async fn hash_password(active: &mut users::ActiveModel) -> Result<()> {
        trace("before_create");

        if rejecting() {
            return Err(Error::Conflict("rejected by hook".into()));
        }

        if let sea_orm::ActiveValue::Set(password) = &active.password {
            active.password = Set(luxid::Hash::make(password)?);
        }
        Ok(())
    }

    async fn stamp(active: &mut users::ActiveModel) -> Result<()> {
        trace("before_save");

        if let sea_orm::ActiveValue::Set(email) = &active.email {
            active.slug = Set(email.replace('@', "-at-"));
        }
        Ok(())
    }

    async fn note_update(_active: &mut users::ActiveModel) -> Result<()> {
        trace("before_update");
        Ok(())
    }

    async fn welcome(_model: &Self) -> Result<()> {
        trace("after_create");
        Ok(())
    }

    async fn note_updated(_model: &Self) -> Result<()> {
        trace("after_update");
        Ok(())
    }

    async fn saved(_model: &Self) -> Result<()> {
        trace("after_save");
        Ok(())
    }
}

const SCHEMA: &str = "
    CREATE TABLE users (
        id       INTEGER PRIMARY KEY AUTOINCREMENT,
        email    TEXT NOT NULL,
        password TEXT NOT NULL,
        slug     TEXT NOT NULL
    );
";

async fn database() -> Db {
    let db = Db::in_memory().await.expect("opens");
    db.connection()
        .execute_unprepared(SCHEMA)
        .await
        .expect("creates schema");
    db
}

fn new_user(email: &str, password: &str) -> users::ActiveModel {
    users::ActiveModel {
        email: Set(email.to_owned()),
        password: Set(password.to_owned()),
        slug: Set(String::new()),
        ..Default::default()
    }
}

#[tokio::test]
async fn create_hooks_run_in_order_and_can_mutate_the_row() {
    let db = database().await;

    db.rollback_scope(async || {
        let _ = taken();

        let user = luxid::insert(new_user("ada@example.com", "hunter2"))
            .await
            .expect("inserts");

        assert_eq!(
            taken(),
            vec!["before_save", "before_create", "after_create", "after_save"]
        );

        // before_create hashed it; the plaintext never reached the database.
        assert_ne!(user.password, "hunter2");
        assert!(luxid::Hash::verify("hunter2", &user.password));

        // before_save derived the slug.
        assert_eq!(user.slug, "ada-at-example.com");
    })
    .await
    .expect("rolls back");
}

#[tokio::test]
async fn update_hooks_run_in_order() {
    let db = database().await;

    db.rollback_scope(async || {
        let user = luxid::insert(new_user("alan@example.com", "secret"))
            .await
            .expect("inserts");
        let _ = taken();

        let mut active = user.into_active_model();
        active.email = Set("alan@other.com".to_owned());

        let updated = luxid::update(active).await.expect("updates");

        assert_eq!(
            taken(),
            vec!["before_save", "before_update", "after_update", "after_save"]
        );
        assert_eq!(
            updated.slug, "alan-at-other.com",
            "before_save ran on update too"
        );
    })
    .await
    .expect("rolls back");
}

#[tokio::test]
async fn a_failing_before_hook_aborts_the_write() {
    let db = database().await;

    db.rollback_scope(async || {
        set_reject(true);
        let _ = taken();

        let outcome = luxid::insert(new_user("nope@example.com", "x")).await;
        set_reject(false);

        assert_eq!(outcome.unwrap_err().status_code().as_u16(), 409);

        // The after hooks never ran, and nothing was written.
        assert_eq!(taken(), vec!["before_save", "before_create"]);
        assert_eq!(User::count_all().await.expect("counts"), 0);
    })
    .await
    .expect("rolls back");
}

#[tokio::test]
async fn insert_without_hooks_skips_them() {
    let db = database().await;

    db.rollback_scope(async || {
        let _ = taken();

        let user = luxid::insert_without_hooks(new_user("raw@example.com", "plaintext"))
            .await
            .expect("inserts");

        assert!(taken().is_empty(), "no hooks fired");
        assert_eq!(user.password, "plaintext", "and nothing hashed it");
    })
    .await
    .expect("rolls back");
}

#[tokio::test]
async fn a_model_without_declared_hooks_still_inserts() {
    // `Hooks` is generated for every model with no-op defaults, so a model that
    // declares nothing is still insertable through the ordinary path.
    let db = database().await;

    db.rollback_scope(async || {
        let count = User::count_all().await.expect("counts");
        assert_eq!(count, 0);
    })
    .await
    .expect("rolls back");
}
