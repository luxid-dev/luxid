//! Batched relation loading.
//!
//! Each relation costs exactly one query regardless of how many parents were
//! fetched — the whole point of eager loading. Called by the code
//! `#[luxid::model]` generates rather than by hand.

use std::collections::HashMap;

use luxid_core::error::Result;
use sea_orm::{EntityTrait, ModelTrait, Value};
use serde::Serialize;

use crate::model::{ColumnRef, Record, Relatable};

/// `Value` is neither `Hash` nor `Ord`, so grouping keys on its debug form is
/// the practical way to bucket parents and children. It is deterministic for a
/// given variant and never leaves this module.
fn group_key(value: &Value) -> String {
    format!("{value:?}")
}

type ColumnOf<T> = <<T as Record>::Entity as EntityTrait>::Column;

/// One parent, many children: `SELECT * FROM children WHERE fk IN (parent ids)`.
pub async fn load_has_many<P, C>(
    parents: &mut [P],
    parent_key: ColumnOf<P>,
    child_key: ColumnOf<C>,
    name: &str,
) -> Result<()>
where
    P: Relatable + ModelTrait<Entity = <P as Record>::Entity>,
    C: Record + ModelTrait<Entity = <C as Record>::Entity> + Clone + Serialize + Send + Sync,
    ColumnOf<C>: ColumnRef<<C as Record>::Entity, Value = Value>,
{
    let keys: Vec<Value> = parents
        .iter()
        .map(|parent| parent.get(parent_key))
        .collect();

    if keys.is_empty() {
        return Ok(());
    }

    let children = C::query().where_in(child_key, keys).all().await?;

    let mut grouped: HashMap<String, Vec<C>> = HashMap::new();
    for child in children {
        grouped
            .entry(group_key(&child.get(child_key)))
            .or_default()
            .push(child);
    }

    for parent in parents.iter_mut() {
        let mine = grouped
            .get(&group_key(&parent.get(parent_key)))
            .cloned()
            .unwrap_or_default();
        parent.relations_mut().insert_many(name, mine);
    }

    Ok(())
}

/// One parent, one child, keyed the same way but yielding a single value.
pub async fn load_has_one<P, C>(
    parents: &mut [P],
    parent_key: ColumnOf<P>,
    child_key: ColumnOf<C>,
    name: &str,
) -> Result<()>
where
    P: Relatable + ModelTrait<Entity = <P as Record>::Entity>,
    C: Record + ModelTrait<Entity = <C as Record>::Entity> + Clone + Serialize + Send + Sync,
    ColumnOf<C>: ColumnRef<<C as Record>::Entity, Value = Value>,
{
    let keys: Vec<Value> = parents
        .iter()
        .map(|parent| parent.get(parent_key))
        .collect();

    if keys.is_empty() {
        return Ok(());
    }

    let children = C::query().where_in(child_key, keys).all().await?;

    let mut first_by_key: HashMap<String, C> = HashMap::new();
    for child in children {
        first_by_key
            .entry(group_key(&child.get(child_key)))
            .or_insert(child);
    }

    for parent in parents.iter_mut() {
        let mine = first_by_key
            .get(&group_key(&parent.get(parent_key)))
            .cloned();
        parent.relations_mut().insert_one(name, mine);
    }

    Ok(())
}

/// The inverse: the parent holds the foreign key.
///
/// Duplicate keys are collapsed before querying, so a hundred posts by three
/// authors fetch three rows.
pub async fn load_belongs_to<P, C>(
    parents: &mut [P],
    foreign_key: ColumnOf<P>,
    owner_key: ColumnOf<C>,
    name: &str,
) -> Result<()>
where
    P: Relatable + ModelTrait<Entity = <P as Record>::Entity>,
    C: Record + ModelTrait<Entity = <C as Record>::Entity> + Clone + Serialize + Send + Sync,
    ColumnOf<C>: ColumnRef<<C as Record>::Entity, Value = Value>,
{
    let mut keys: Vec<Value> = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();

    for parent in parents.iter() {
        let value = parent.get(foreign_key);

        if seen.insert(group_key(&value), ()).is_none() {
            keys.push(value);
        }
    }

    if keys.is_empty() {
        return Ok(());
    }

    let owners = C::query().where_in(owner_key, keys).all().await?;

    let mut by_key: HashMap<String, C> = HashMap::new();
    for owner in owners {
        by_key.insert(group_key(&owner.get(owner_key)), owner);
    }

    for parent in parents.iter_mut() {
        let mine = by_key.get(&group_key(&parent.get(foreign_key))).cloned();
        parent.relations_mut().insert_one(name, mine);
    }

    Ok(())
}
