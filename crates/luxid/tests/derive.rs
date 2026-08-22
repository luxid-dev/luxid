//! `#[derive(Model)]` behaviour: name derivation, ignored fields, optional
//! columns, and the untyped escape hatch.

use luxid::prelude::*;
use sea_orm::{ActiveValue::Set, ConnectionTrait};

/// A plural table name, plus an `#[sea_orm(ignore)]` field standing in for the
/// relations bag.
mod categories {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, luxid::Model)]
    #[sea_orm(table_name = "categories")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub name: String,
        pub parent_id: Option<i64>,

        /// Not a column. The derive must skip it, and SeaORM must not map it.
        #[sea_orm(ignore)]
        pub loaded_children: Vec<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// An irregular plural, named explicitly.
mod people {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, luxid::Model)]
    #[luxid(name = "Person")]
    #[sea_orm(table_name = "people")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub full_name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

use categories::Model as Category;
use people::Model as Person;

const SCHEMA: &str = "
    CREATE TABLE categories (
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        name      TEXT    NOT NULL,
        parent_id INTEGER
    );
    CREATE TABLE people (
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        full_name TEXT    NOT NULL
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

#[tokio::test]
async fn the_model_name_is_singularized_from_the_table_name() {
    let db = database().await;

    db.scope(async {
        let err = Category::find_or_fail(7).await.unwrap_err();
        assert_eq!(
            err.problem()["resource"],
            "Category",
            "categories → Category"
        );
    })
    .await;
}

#[tokio::test]
async fn an_explicit_name_overrides_the_derived_one() {
    let db = database().await;

    db.scope(async {
        let err = Person::find_or_fail(7).await.unwrap_err();
        assert_eq!(
            err.problem()["resource"],
            "Person",
            "people would not singularize"
        );
    })
    .await;
}

#[tokio::test]
async fn ignored_fields_are_not_columns_but_still_exist_on_the_model() {
    let db = database().await;

    db.scope(async {
        luxid::insert(categories::ActiveModel {
            name: Set("Rust".to_owned()),
            parent_id: Set(None),
            ..Default::default()
        })
        .await
        .expect("inserts");

        let category = Category::query()
            .where_eq(Category::name, "Rust")
            .first()
            .await
            .expect("queries")
            .expect("exists");

        // Present on the struct, absent from the table — this is where eager
        // loaded relations will live.
        assert!(category.loaded_children.is_empty());
    })
    .await;
}

#[tokio::test]
async fn optional_columns_compare_against_the_inner_type() {
    let db = database().await;

    db.scope(async {
        let root = luxid::insert(categories::ActiveModel {
            name: Set("Root".to_owned()),
            parent_id: Set(None),
            ..Default::default()
        })
        .await
        .expect("inserts");

        luxid::insert(categories::ActiveModel {
            name: Set("Child".to_owned()),
            parent_id: Set(Some(root.id)),
            ..Default::default()
        })
        .await
        .expect("inserts");

        // `parent_id` is `Option<i64>`; it compares against `i64`.
        let children = Category::query()
            .where_eq(Category::parent_id, root.id)
            .all()
            .await
            .expect("queries");

        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "Child");

        // Absence is expressed with where_null, not by comparing to a null.
        let roots = Category::query()
            .where_null(Category::parent_id)
            .all()
            .await
            .expect("queries");

        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "Root");
    })
    .await;
}

#[tokio::test]
async fn typed_and_untyped_columns_are_interchangeable() {
    let db = database().await;

    db.scope(async {
        luxid::insert(categories::ActiveModel {
            name: Set("Shared".to_owned()),
            parent_id: Set(None),
            ..Default::default()
        })
        .await
        .expect("inserts");

        let typed = Category::query()
            .where_eq(Category::name, "Shared")
            .count()
            .await;
        let untyped = Category::query()
            .where_eq(categories::Column::Name, "Shared")
            .count()
            .await;

        assert_eq!(typed.expect("counts"), 1);
        assert_eq!(untyped.expect("counts"), 1);
    })
    .await;
}

#[tokio::test]
async fn where_in_and_ordering_use_typed_columns() {
    let db = database().await;

    db.scope(async {
        for name in ["Alpha", "Beta", "Gamma"] {
            luxid::insert(categories::ActiveModel {
                name: Set(name.to_owned()),
                parent_id: Set(None),
                ..Default::default()
            })
            .await
            .expect("inserts");
        }

        let found = Category::query()
            .where_in(Category::name, ["Alpha", "Gamma"])
            .order_by_desc(Category::name)
            .all()
            .await
            .expect("queries");

        let names: Vec<_> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Gamma", "Alpha"]);
    })
    .await;
}
