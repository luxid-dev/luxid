//! Eloquent-flavoured model operations over SeaORM entities.

use luxid_core::error::{Error, Result};
use sea_orm::{
    ActiveModelBehavior, ActiveModelTrait, ColumnTrait, EntityTrait, FromQueryResult,
    IntoActiveModel, Order, PaginatorTrait, PrimaryKeyTrait, QueryFilter, QueryOrder, QuerySelect,
    Select, Value,
};
use serde::Serialize;

use crate::db;
use crate::hooks::Hooks;
use crate::relations::Relations;
use crate::with_connection;
use luxid_core::middleware::BoxFuture;

/// The primary-key value type of an entity.
pub type PrimaryKeyOf<E> = <<E as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType;

pub(crate) fn database_error(err: sea_orm::DbErr) -> Error {
    Error::internal(format!("database error: {err}"))
}

/// A column a query can filter or order by.
///
/// Two kinds implement this. The entity's own `Column` enum carries
/// `Value = sea_orm::Value`, so it accepts anything — the escape hatch.
/// `#[derive(Model)]` also generates a zero-sized type per field whose `Value`
/// is that field's actual Rust type, so `where_eq(User::team_id, "abc")` fails
/// to compile where the enum form would only fail at runtime.
pub trait ColumnRef<E: EntityTrait> {
    /// What this column may be compared against.
    type Value;

    fn column(&self) -> E::Column;
}

/// A page of results. Serializes in the shape Laravel's paginator produces, so
/// existing clients need no translation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Paginated<T> {
    pub data: Vec<T>,
    pub page: u64,
    pub per_page: u64,
    pub total: u64,
    pub last_page: u64,
}

impl<T> Paginated<T> {
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn has_more(&self) -> bool {
        self.page < self.last_page
    }
}

/// A query under construction.
/// An eager-load step, captured when `.with(..)` is called so the rest of the
/// query API needs no `Relatable` bound.
type Loader<M> = Box<dyn for<'a> Fn(&'a mut Vec<M>) -> BoxFuture<'a, Result<()>> + Send + Sync>;

pub struct Query<E: EntityTrait> {
    select: Select<E>,
    loaders: Vec<Loader<E::Model>>,
}

impl<E> Query<E>
where
    E: EntityTrait,
    E::Model: FromQueryResult + Send + Sync,
{
    pub fn new(select: Select<E>) -> Self {
        Self {
            select,
            loaders: Vec::new(),
        }
    }

    /// Escape hatch: reach the underlying SeaORM `Select` for anything the data
    /// layer does not express.
    pub fn into_inner(self) -> Select<E> {
        self.select
    }

    #[must_use]
    pub fn where_eq<C, V>(mut self, column: C, value: V) -> Self
    where
        C: ColumnRef<E>,
        V: Into<C::Value>,
        C::Value: Into<Value>,
    {
        let target = column.column();
        self.select = self.select.filter(target.eq(value.into().into()));
        self
    }

    #[must_use]
    pub fn where_ne<C, V>(mut self, column: C, value: V) -> Self
    where
        C: ColumnRef<E>,
        V: Into<C::Value>,
        C::Value: Into<Value>,
    {
        let target = column.column();
        self.select = self.select.filter(target.ne(value.into().into()));
        self
    }

    #[must_use]
    pub fn where_gt<C, V>(mut self, column: C, value: V) -> Self
    where
        C: ColumnRef<E>,
        V: Into<C::Value>,
        C::Value: Into<Value>,
    {
        let target = column.column();
        self.select = self.select.filter(target.gt(value.into().into()));
        self
    }

    #[must_use]
    pub fn where_lt<C, V>(mut self, column: C, value: V) -> Self
    where
        C: ColumnRef<E>,
        V: Into<C::Value>,
        C::Value: Into<Value>,
    {
        let target = column.column();
        self.select = self.select.filter(target.lt(value.into().into()));
        self
    }

    #[must_use]
    pub fn where_in<C, V>(mut self, column: C, values: impl IntoIterator<Item = V>) -> Self
    where
        C: ColumnRef<E>,
        V: Into<C::Value>,
        C::Value: Into<Value>,
    {
        let target = column.column();
        let values: Vec<Value> = values.into_iter().map(|v| v.into().into()).collect();

        self.select = self.select.filter(target.is_in(values));
        self
    }

    #[must_use]
    pub fn where_like<C>(mut self, column: C, pattern: impl Into<String>) -> Self
    where
        C: ColumnRef<E>,
    {
        let target = column.column();
        self.select = self.select.filter(target.like(pattern.into()));
        self
    }

    #[must_use]
    pub fn where_null<C: ColumnRef<E>>(mut self, column: C) -> Self {
        let target = column.column();
        self.select = self.select.filter(target.is_null());
        self
    }

    #[must_use]
    pub fn where_not_null<C: ColumnRef<E>>(mut self, column: C) -> Self {
        let target = column.column();
        self.select = self.select.filter(target.is_not_null());
        self
    }

    #[must_use]
    pub fn order_by_asc<C: ColumnRef<E>>(mut self, column: C) -> Self {
        let target = column.column();
        self.select = self.select.order_by(target, Order::Asc);
        self
    }

    #[must_use]
    pub fn order_by_desc<C: ColumnRef<E>>(mut self, column: C) -> Self {
        let target = column.column();
        self.select = self.select.order_by(target, Order::Desc);
        self
    }

    #[must_use]
    pub fn limit(mut self, limit: u64) -> Self {
        self.select = self.select.limit(limit);
        self
    }

    #[must_use]
    pub fn offset(mut self, offset: u64) -> Self {
        self.select = self.select.offset(offset);
        self
    }

    pub async fn all(self) -> Result<Vec<E::Model>> {
        let Self { select, loaders } = self;
        let handle = db::current()?;

        let mut rows = with_connection!(handle, |conn| select
            .all(conn)
            .await
            .map_err(database_error))?;

        load_all(&loaders, &mut rows).await?;
        Ok(rows)
    }

    pub async fn first(self) -> Result<Option<E::Model>> {
        let Self { select, loaders } = self;
        let handle = db::current()?;

        let found = with_connection!(handle, |conn| select
            .one(conn)
            .await
            .map_err(database_error))?;

        let Some(row) = found else {
            return Ok(None);
        };

        // Relations load in batches, so a single row goes through the same
        // path as a page of them.
        let mut rows = vec![row];
        load_all(&loaders, &mut rows).await?;

        Ok(rows.pop())
    }

    /// The first row, or a 404 naming the model.
    pub async fn first_or_fail(self) -> Result<E::Model>
    where
        E::Model: Record<Entity = E>,
    {
        self.first()
            .await?
            .ok_or_else(|| Error::not_found(<E::Model as Record>::MODEL, "?"))
    }

    pub async fn count(self) -> Result<u64> {
        let handle = db::current()?;
        let select = self.select;

        with_connection!(handle, |conn| select
            .count(conn)
            .await
            .map_err(database_error))
    }

    pub async fn exists(self) -> Result<bool> {
        Ok(self.count().await? > 0)
    }

    /// A page of results. `page` is 1-based, matching Laravel and every URL
    /// users will ever type.
    pub async fn paginate(self, page: u64, per_page: u64) -> Result<Paginated<E::Model>> {
        let per_page = per_page.max(1);
        let page = page.max(1);

        let handle = db::current()?;

        let Self { select, loaders } = self;

        let (mut data, counts) = with_connection!(handle, |conn| {
            let paginator = select.paginate(conn, per_page);

            let counts = paginator
                .num_items_and_pages()
                .await
                .map_err(database_error)?;
            let data = paginator
                .fetch_page(page - 1)
                .await
                .map_err(database_error)?;

            Ok::<_, Error>((data, counts))
        })?;

        load_all(&loaders, &mut data).await?;

        Ok(Paginated {
            data,
            page,
            per_page,
            total: counts.number_of_items,
            last_page: counts.number_of_pages.max(1),
        })
    }
}

impl<E> Query<E>
where
    E: EntityTrait,
    E::Model: Relatable + FromQueryResult + Send + Sync,
{
    /// Eager-load a relation declared by `#[luxid::model]`.
    ///
    /// Loading happens in one batched query per relation after the parents are
    /// fetched, which is what makes this an N+1 fix rather than a rename of
    /// one.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>) -> Self {
        let name = name.into();

        self.loaders.push(Box::new(move |rows| {
            E::Model::load_relation(name.clone(), rows)
        }));
        self
    }
}

async fn load_all<M>(loaders: &[Loader<M>], rows: &mut Vec<M>) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }

    for loader in loaders {
        loader(rows).await?;
    }
    Ok(())
}

/// A model that declares relations. Implemented by `#[luxid::model]`.
pub trait Relatable: Record {
    fn relations(&self) -> &Relations;

    fn relations_mut(&mut self) -> &mut Relations;

    /// Load `name` onto every parent, in one batched query.
    fn load_relation(name: String, parents: &mut Vec<Self>) -> BoxFuture<'_, Result<()>>;
}

/// Read operations on a stored row, implemented by `#[derive(Model)]`.
///
/// One impl per entity model, which is what lets `User::find(id)` and
/// `User::query()` exist without a handle being threaded through.
pub trait Record: FromQueryResult + Send + Sync + Sized + 'static {
    type Entity: EntityTrait<Model = Self>;

    /// Name used in 404s and diagnostics.
    const MODEL: &'static str;

    fn query() -> Query<Self::Entity> {
        Query::new(Self::Entity::find())
    }

    fn find(id: PrimaryKeyOf<Self::Entity>) -> impl Future<Output = Result<Option<Self>>> + Send {
        async move {
            let handle = db::current()?;
            let select = Self::Entity::find_by_id(id);

            with_connection!(handle, |conn| select
                .one(conn)
                .await
                .map_err(database_error))
        }
    }

    /// The row, or a 404 carrying the model name and id — the line that keeps
    /// controller actions free of error handling.
    fn find_or_fail(id: PrimaryKeyOf<Self::Entity>) -> impl Future<Output = Result<Self>> + Send
    where
        PrimaryKeyOf<Self::Entity>: Clone + std::fmt::Display,
    {
        async move {
            let requested = id.clone();

            Self::find(id)
                .await?
                .ok_or_else(|| Error::not_found(Self::MODEL, requested))
        }
    }

    fn all() -> impl Future<Output = Result<Vec<Self>>> + Send {
        async move { Self::query().all().await }
    }

    fn count_all() -> impl Future<Output = Result<u64>> + Send {
        async move { Self::query().count().await }
    }
}

/// Insert an active model and return the stored row, running the model's
/// create hooks around the write.
pub async fn insert<A>(active: A) -> Result<<A::Entity as EntityTrait>::Model>
where
    A: ActiveModelTrait + ActiveModelBehavior + Send,
    <A::Entity as EntityTrait>::Model: IntoActiveModel<A> + Hooks<Active = A>,
{
    type Model<A> = <<A as ActiveModelTrait>::Entity as EntityTrait>::Model;

    let mut active = active;

    Model::<A>::before_save(&mut active).await?;
    Model::<A>::before_create(&mut active).await?;

    let handle = db::current()?;
    let stored = with_connection!(handle, |conn| active
        .insert(conn)
        .await
        .map_err(database_error))?;

    Model::<A>::after_create(&stored).await?;
    Model::<A>::after_save(&stored).await?;

    Ok(stored)
}

/// Persist changes to an active model and return the stored row, running the
/// model's update hooks around the write.
pub async fn update<A>(active: A) -> Result<<A::Entity as EntityTrait>::Model>
where
    A: ActiveModelTrait + ActiveModelBehavior + Send,
    <A::Entity as EntityTrait>::Model: IntoActiveModel<A> + Hooks<Active = A>,
{
    type Model<A> = <<A as ActiveModelTrait>::Entity as EntityTrait>::Model;

    let mut active = active;

    Model::<A>::before_save(&mut active).await?;
    Model::<A>::before_update(&mut active).await?;

    let handle = db::current()?;
    let stored = with_connection!(handle, |conn| active
        .update(conn)
        .await
        .map_err(database_error))?;

    Model::<A>::after_update(&stored).await?;
    Model::<A>::after_save(&stored).await?;

    Ok(stored)
}

/// Insert without running hooks.
///
/// Named for what it costs you. Reach for it in seeders and fixtures where
/// hooks would be wrong, never in application code.
pub async fn insert_without_hooks<A>(active: A) -> Result<<A::Entity as EntityTrait>::Model>
where
    A: ActiveModelTrait + ActiveModelBehavior + Send,
    <A::Entity as EntityTrait>::Model: IntoActiveModel<A>,
{
    let handle = db::current()?;
    with_connection!(handle, |conn| active
        .insert(conn)
        .await
        .map_err(database_error))
}

/// Delete a row by primary key. Returns whether anything was removed.
pub async fn delete_by_id<E: EntityTrait>(id: PrimaryKeyOf<E>) -> Result<bool> {
    let handle = db::current()?;

    with_connection!(handle, |conn| {
        E::delete_by_id(id)
            .exec(conn)
            .await
            .map(|outcome| outcome.rows_affected > 0)
            .map_err(database_error)
    })
}
