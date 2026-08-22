//! Model factories.
//!
//! A factory says what a *typical* row looks like; a test says only what it
//! cares about. That is the whole point — a test that spells out every column
//! is a test that breaks when an unrelated column is added.
//!
//! ```ignore
//! UserFactory::new().create().await?;                    // one typical user
//! UserFactory::new().count(3).create().await?;           // three of them
//! UserFactory::new()
//!     .state(|user| user.email = Set("ada@example.com".into()))
//!     .create_one()
//!     .await?;                                           // one, with an override
//! ```

use luxid_core::error::Result;
use sea_orm::{ActiveModelBehavior, ActiveModelTrait, EntityTrait, IntoActiveModel};

use crate::hooks::Hooks;
use crate::model::insert;

/// The typical shape of a row.
///
/// `#[luxid::factory]`-generated types implement this; `new()` gives the
/// builder.
pub trait Factory: Sized {
    type Active: ActiveModelTrait + ActiveModelBehavior + Send;

    /// A row with every required column filled in.
    fn definition() -> Self::Active;

    /// Start building.
    fn new() -> FactoryBuilder<Self> {
        FactoryBuilder::new()
    }
}

type ModelOf<F> = <<<F as Factory>::Active as ActiveModelTrait>::Entity as EntityTrait>::Model;
type Mutation<F> = Box<dyn Fn(&mut <F as Factory>::Active) + Send>;

/// A pending set of rows.
pub struct FactoryBuilder<F: Factory> {
    count: usize,
    /// Applied in order, so a later override wins.
    states: Vec<Mutation<F>>,
}

impl<F: Factory> Default for FactoryBuilder<F> {
    fn default() -> Self {
        Self::new()
    }
}

impl<F: Factory> FactoryBuilder<F> {
    pub fn new() -> Self {
        Self {
            count: 1,
            states: Vec::new(),
        }
    }

    /// How many rows to build.
    #[must_use]
    pub fn count(mut self, count: usize) -> Self {
        self.count = count;
        self
    }

    /// Override part of the definition.
    ///
    /// The closure receives each row in turn, so a `count(3)` with a state that
    /// varies by call produces three different rows.
    #[must_use]
    pub fn state(mut self, state: impl Fn(&mut F::Active) + Send + 'static) -> Self {
        self.states.push(Box::new(state));
        self
    }

    /// Build the active models without touching the database.
    pub fn make(&self) -> Vec<F::Active> {
        (0..self.count)
            .map(|_| {
                let mut active = F::definition();

                for state in &self.states {
                    state(&mut active);
                }

                active
            })
            .collect()
    }

    /// Insert the rows, running the model's hooks.
    pub async fn create(&self) -> Result<Vec<ModelOf<F>>>
    where
        ModelOf<F>: IntoActiveModel<F::Active> + Hooks<Active = F::Active>,
    {
        let mut created = Vec::with_capacity(self.count);

        // Sequentially, because rows often depend on the ids of earlier ones
        // and because a factory is not a throughput problem.
        for active in self.make() {
            created.push(insert(active).await?);
        }

        Ok(created)
    }

    /// Insert exactly one row and return it, regardless of `count`.
    pub async fn create_one(&self) -> Result<ModelOf<F>>
    where
        ModelOf<F>: IntoActiveModel<F::Active> + Hooks<Active = F::Active>,
    {
        let mut active = F::definition();

        for state in &self.states {
            state(&mut active);
        }

        insert(active).await
    }
}
