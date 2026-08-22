//! The middleware chain.
//!
//! Luxid owns the chain rather than delegating to salvo's hoops. The reason is
//! the owning `HttpContext`: salvo's middleware receive borrowed request parts,
//! so a mutation made there (setting the authenticated user, say) could not be
//! carried into an action that owns its context. Building the context once and
//! threading it through a Luxid-owned chain keeps one context per request and
//! makes mutations visible downstream.
//!
//! Middleware and actions share the same `HttpContext` type, so there is one
//! mental model for the whole framework.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::context::HttpContext;
use crate::error::Result;
use crate::http::Response;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The terminal step of a chain: a controller action.
///
/// Implemented by the zero-sized types `#[luxid::controller]` generates.
pub trait Action: Send + Sync + 'static {
    fn call(&self, ctx: HttpContext) -> BoxFuture<'static, Result<Response>>;

    /// What `luxid routes` prints for this action.
    ///
    /// `#[luxid::controller]` overrides this with `Controller::action`; the
    /// default is the generated type's name, which is at least findable.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// What `#[openapi(..)]` recorded, if anything.
    ///
    /// The action owns its documentation rather than a global registry owning
    /// it, so the spec is assembled from the routes that actually exist.
    fn openapi(&self) -> Option<crate::openapi::Operation> {
        None
    }
}

/// A step in the chain.
///
/// Code before `next.run()` runs on the way in, code after runs on the way out,
/// and returning early short-circuits — so there is no separate before/after
/// API to learn.
pub trait Middleware: Send + Sync + 'static {
    fn handle<'a>(&'a self, ctx: HttpContext, next: Next) -> BoxFuture<'a, Result<Response>>;
}

/// The remainder of the chain, handed to each middleware.
pub struct Next {
    stack: Arc<[Arc<dyn Middleware>]>,
    index: usize,
    action: Arc<dyn Action>,
}

impl Next {
    pub(crate) fn new(stack: Arc<[Arc<dyn Middleware>]>, action: Arc<dyn Action>) -> Self {
        Self {
            stack,
            index: 0,
            action,
        }
    }

    /// Run the rest of the chain. Recursion terminates in the action.
    pub async fn run(mut self, ctx: HttpContext) -> Result<Response> {
        // The type recursion this would otherwise create is broken by the
        // trait objects, whose futures are already boxed.
        match self.stack.get(self.index).cloned() {
            Some(middleware) => {
                self.index += 1;
                middleware.handle(ctx, self).await
            }
            None => self.action.call(ctx).await,
        }
    }
}

impl std::fmt::Debug for Next {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Next")
            .field("remaining", &(self.stack.len().saturating_sub(self.index)))
            .finish()
    }
}
