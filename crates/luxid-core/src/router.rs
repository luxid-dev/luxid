//! Explicit route registration.
//!
//! Routes are declared, never auto-discovered. A route that does not exist must
//! be traceable to a visible line of code rather than to a linker section that
//! silently dropped a symbol.
//!
//! Middleware attaches at three levels and runs outermost-first:
//! global (`App::middleware`) → group (`Router::middleware`) → route
//! (`.middleware()` on the returned handle).

use std::sync::Arc;

use crate::middleware::{Action, Middleware};

/// A controller that defines resource actions.
///
/// Implemented by `#[luxid::controller]` for any block defining at least one of
/// `index`, `show`, `store`, `update`, `destroy` — and it registers only the
/// ones that exist, so a read-only controller does not acquire a `DELETE` route
/// that 500s.
pub trait ResourceRoutes {
    fn register(router: &mut Router);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

pub struct Route {
    method: Method,
    path: String,
    action: Arc<dyn Action>,
    middleware: Vec<Arc<dyn Middleware>>,
}

impl Route {
    /// Attach middleware to this route alone. Chainable.
    pub fn middleware<M: Middleware>(&mut self, middleware: M) -> &mut Self {
        self.middleware.push(Arc::new(middleware));
        self
    }
}

enum Entry {
    Route(Route),
    Group { prefix: String, router: Router },
}

/// One row of the routing table, as `luxid routes` renders it.
///
/// Not comparable: it carries an `Operation`, which holds schema function
/// pointers. Compare the fields you care about, or the rendered document.
#[derive(Debug, Clone)]
pub struct RouteInfo {
    pub method: Method,
    pub path: String,
    pub action: &'static str,
    /// How many middleware wrap this route, outermost first.
    pub middleware: usize,
    /// What `#[openapi(..)]` recorded for this action.
    pub operation: Option<crate::openapi::Operation>,
}

impl Method {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

/// A flattened route, used by `App` and by `luxid routes`.
pub struct Registered {
    pub method: Method,
    pub path: String,
    pub action: Arc<dyn Action>,
    pub middleware: Vec<Arc<dyn Middleware>>,
}

#[derive(Default)]
pub struct Router {
    entries: Vec<Entry>,
    middleware: Vec<Arc<dyn Middleware>>,
}

impl Router {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach middleware to every route in this router (and its groups).
    pub fn middleware<M: Middleware>(&mut self, middleware: M) -> &mut Self {
        self.middleware.push(Arc::new(middleware));
        self
    }

    pub fn get<A: Action>(&mut self, path: &str, action: A) -> &mut Route {
        self.route(Method::Get, path, action)
    }

    pub fn post<A: Action>(&mut self, path: &str, action: A) -> &mut Route {
        self.route(Method::Post, path, action)
    }

    pub fn put<A: Action>(&mut self, path: &str, action: A) -> &mut Route {
        self.route(Method::Put, path, action)
    }

    pub fn patch<A: Action>(&mut self, path: &str, action: A) -> &mut Route {
        self.route(Method::Patch, path, action)
    }

    pub fn delete<A: Action>(&mut self, path: &str, action: A) -> &mut Route {
        self.route(Method::Delete, path, action)
    }

    /// Register a controller's resource actions under `path`.
    ///
    /// ```ignore
    /// r.resource("/posts", PostsController).middleware(Auth::jwt());
    /// ```
    ///
    /// Returns the group the routes live in, so middleware attaches to all of
    /// them at once.
    pub fn resource<C: ResourceRoutes>(&mut self, path: &str, controller: C) -> &mut Router {
        let _ = controller;

        let mut inner = Router::new();
        C::register(&mut inner);

        self.entries.push(Entry::Group {
            prefix: path.to_owned(),
            router: inner,
        });

        match self.entries.last_mut() {
            Some(Entry::Group { router, .. }) => router,
            _ => unreachable!("a group was just pushed"),
        }
    }

    /// Nest routes under a shared prefix. Call `r.middleware(..)` inside the
    /// closure to apply middleware to the whole group.
    pub fn group(&mut self, prefix: &str, build: impl FnOnce(&mut Router)) -> &mut Self {
        let mut inner = Router::new();
        build(&mut inner);

        self.entries.push(Entry::Group {
            prefix: prefix.to_owned(),
            router: inner,
        });
        self
    }

    fn route<A: Action>(&mut self, method: Method, path: &str, action: A) -> &mut Route {
        self.entries.push(Entry::Route(Route {
            method,
            path: path.to_owned(),
            action: Arc::new(action),
            middleware: Vec::new(),
        }));

        match self.entries.last_mut() {
            Some(Entry::Route(route)) => route,
            _ => unreachable!("a route was just pushed"),
        }
    }

    /// Describe the routing table without consuming the router, so an app can
    /// print its routes and then serve them.
    pub fn describe(&self) -> Vec<RouteInfo> {
        let mut rows = Vec::new();
        self.describe_into("", self.middleware.len(), &mut rows);
        rows
    }

    fn describe_into(&self, prefix: &str, inherited: usize, rows: &mut Vec<RouteInfo>) {
        for entry in &self.entries {
            match entry {
                Entry::Route(route) => rows.push(RouteInfo {
                    method: route.method,
                    path: join(prefix, &route.path),
                    action: route.action.name(),
                    middleware: inherited + route.middleware.len(),
                    operation: route.action.openapi(),
                }),
                Entry::Group {
                    prefix: group_prefix,
                    router,
                } => router.describe_into(
                    &join(prefix, group_prefix),
                    inherited + router.middleware.len(),
                    rows,
                ),
            }
        }
    }

    /// Flatten into absolute paths with fully resolved middleware stacks.
    pub(crate) fn flatten(self) -> Vec<Registered> {
        self.flatten_with("", &[])
    }

    fn flatten_with(self, prefix: &str, inherited: &[Arc<dyn Middleware>]) -> Vec<Registered> {
        let mut stack = inherited.to_vec();
        stack.extend(self.middleware);

        let mut registered = Vec::new();

        for entry in self.entries {
            match entry {
                Entry::Route(route) => {
                    let mut middleware = stack.clone();
                    middleware.extend(route.middleware);

                    registered.push(Registered {
                        method: route.method,
                        path: join(prefix, &route.path),
                        action: route.action,
                        middleware,
                    });
                }
                Entry::Group {
                    prefix: group_prefix,
                    router,
                } => {
                    registered.extend(router.flatten_with(&join(prefix, &group_prefix), &stack));
                }
            }
        }

        registered
    }
}

fn join(prefix: &str, path: &str) -> String {
    let head = prefix.trim_end_matches('/');
    let tail = path.trim_start_matches('/');

    // `resource` registers its collection route as "", which must join to the
    // group prefix itself rather than to `/posts/`.
    let joined = if tail.is_empty() {
        head.to_owned()
    } else {
        format!("{head}/{tail}")
    };

    if joined.is_empty() {
        "/".to_owned()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::HttpContext;
    use crate::error::Result;
    use crate::http::Response;
    use crate::middleware::{BoxFuture, Next};

    struct Noop;
    impl Action for Noop {
        fn call(&self, ctx: HttpContext) -> BoxFuture<'static, Result<Response>> {
            Box::pin(async move { ctx.response.no_content() })
        }
    }

    struct Tag;
    impl Middleware for Tag {
        fn handle<'a>(&'a self, ctx: HttpContext, next: Next) -> BoxFuture<'a, Result<Response>> {
            Box::pin(async move { next.run(ctx).await })
        }
    }

    #[test]
    fn joins_group_prefixes_into_absolute_paths() {
        let mut router = Router::new();
        router.group("/api/v1", |r| {
            r.get("/users", Noop);
            r.group("/admin", |r| {
                r.get("/stats", Noop);
            });
        });

        let paths: Vec<_> = router.flatten().into_iter().map(|r| r.path).collect();
        assert_eq!(paths, vec!["/api/v1/users", "/api/v1/admin/stats"]);
    }

    #[test]
    fn resolves_middleware_outermost_first() {
        let mut router = Router::new();
        router.middleware(Tag);
        router.group("/api", |r| {
            r.middleware(Tag);
            r.get("/users", Noop).middleware(Tag);
            r.get("/health", Noop);
        });

        let registered = router.flatten();

        assert_eq!(registered[0].middleware.len(), 3, "global + group + route");
        assert_eq!(registered[1].middleware.len(), 2, "global + group only");
    }

    #[test]
    fn a_route_at_the_root_keeps_a_single_slash() {
        let mut router = Router::new();
        router.get("/", Noop);

        assert_eq!(router.flatten()[0].path, "/");
    }

    #[test]
    fn sibling_groups_do_not_inherit_each_others_middleware() {
        let mut router = Router::new();
        router.group("/a", |r| {
            r.middleware(Tag);
            r.get("/x", Noop);
        });
        router.group("/b", |r| {
            r.get("/y", Noop);
        });

        let registered = router.flatten();
        assert_eq!(registered[0].middleware.len(), 1);
        assert_eq!(registered[1].middleware.len(), 0);
    }
}
