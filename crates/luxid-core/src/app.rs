//! Application assembly and startup.

use crate::adapter::RouteHandler;
use crate::config::Config;
use crate::container::{Container, Providers};
use crate::error::Result;
use crate::middleware::Middleware;
use crate::router::{Method, RouteInfo, Router};

const DEFAULT_ADDR: &str = "127.0.0.1:3000";

pub struct App {
    router: Router,
    addr: String,
    providers: Providers,
    config: Config,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            router: Router::new(),
            addr: default_addr(),
            providers: Providers::new(),
            // Environment only until told otherwise; `luxid new` generates an
            // `app::build` that loads `luxid.toml` over it.
            config: Config::from_env(),
        }
    }

    #[must_use]
    pub fn bind(mut self, addr: impl Into<String>) -> Self {
        self.addr = addr.into();
        self
    }

    /// Configuration available to every action as `ctx.config`.
    #[must_use]
    pub fn config(mut self, config: Config) -> Self {
        self.config = config;
        self
    }

    /// Service bindings for the application.
    #[must_use]
    pub fn providers(mut self, providers: Providers) -> Self {
        self.providers = providers;
        self
    }

    /// Middleware applied to every route, outermost first.
    #[must_use]
    pub fn middleware<M: Middleware>(mut self, middleware: M) -> Self {
        self.router.middleware(middleware);
        self
    }

    #[must_use]
    pub fn routes(mut self, register: impl FnOnce(&mut Router)) -> Self {
        register(&mut self.router);
        self
    }

    /// The resolved routing table. Backs `luxid routes`, so an unexpected 404
    /// is answerable without reading the source.
    pub fn route_table(&self) -> Vec<RouteInfo> {
        self.router.describe()
    }

    /// The OpenAPI 3.1 document for this application's routes.
    pub fn openapi(&self, title: &str, version: &str) -> serde_json::Value {
        crate::openapi::document(title, version, &self.route_table())
    }

    /// Build the service without binding a port, so tests exercise the same
    /// routing, container and adapter code as production.
    ///
    /// This deliberately skips eager singleton resolution: a test that binds
    /// only the services it needs should not have to satisfy the whole
    /// application graph. Use [`App::try_into_service`] when you want that
    /// validation.
    pub fn into_service(self) -> salvo::Service {
        salvo::Service::new(self.into_parts().0)
    }

    /// As [`App::into_service`], but validates the provider graph first.
    pub fn try_into_service(self) -> Result<salvo::Service> {
        let (router, services) = self.into_parts();
        services.eager_init()?;

        Ok(salvo::Service::new(router))
    }

    pub async fn run(self) -> Result<()> {
        let addr = self.addr.clone();
        let (router, services) = self.into_parts();

        // Validate the graph before taking the port, so a misconfigured app
        // fails at startup naming the type rather than 500-ing on first
        // request.
        services.eager_init()?;

        let acceptor = {
            use salvo::conn::Listener as _;
            salvo::conn::TcpListener::new(addr.clone()).bind().await
        };

        println!("luxid listening on http://{addr}");
        salvo::Server::new(acceptor).serve(router).await;

        Ok(())
    }

    /// The router and container this app assembles into.
    ///
    /// The CLI needs the container to reach the database without starting a
    /// server.
    pub fn into_parts(self) -> (salvo::Router, Container) {
        let services = self.providers.build();
        let mut root = salvo::Router::new();

        let flattened = self.router.flatten();

        // Static mounts first: salvo matches in registration order, and a
        // wildcard asset route must not shadow the application's own routes.
        for mount in flattened.statics {
            root = root.push(
                salvo::Router::with_path(format!("{}/{{**path}}", mount.path))
                    .get(salvo::serve_static::StaticDir::new([mount.dir]).auto_list(false)),
            );
        }

        for route in flattened.routes {
            let handler = RouteHandler::new(
                route.middleware,
                route.action,
                services.clone(),
                self.config.clone(),
            );
            let leaf = salvo::Router::with_path(&route.path);

            let leaf = match route.method {
                Method::Get => leaf.get(handler),
                Method::Post => leaf.post(handler),
                Method::Put => leaf.put(handler),
                Method::Patch => leaf.patch(handler),
                Method::Delete => leaf.delete(handler),
                Method::Options => leaf.options(handler),
            };

            root = root.push(leaf);
        }

        (root, services)
    }
}

/// `LUXID_ADDR` wins; otherwise `PORT` on all interfaces (the shape most
/// platforms inject); otherwise localhost:3000.
fn default_addr() -> String {
    if let Ok(addr) = std::env::var("LUXID_ADDR") {
        return addr;
    }
    if let Ok(port) = std::env::var("PORT") {
        return format!("0.0.0.0:{port}");
    }
    DEFAULT_ADDR.to_owned()
}
