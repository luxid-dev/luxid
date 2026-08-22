//! The salvo bridge. This is the only module in Luxid that knows salvo exists.
//!
//! Inbound: salvo's borrowed request is copied into an owning Luxid
//! `HttpContext`. Outbound: the `Result<Response>` an action returns is written
//! back onto salvo's response, with `Err` routed through the RFC 7807 renderer.

use std::collections::HashMap;
use std::sync::Arc;

use crate::config::Config;
use crate::container::Container;
use crate::context::{HttpContext, Params};
use crate::error::Error;
use crate::http::{Body, Request, Response};
use crate::middleware::{Action, Middleware, Next};

/// Build an owning context from salvo's borrowed parts.
pub async fn build_context(
    req: &mut salvo::Request,
    services: Container,
    config: Config,
) -> HttpContext {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let headers = req.headers().clone();

    let mut queries: HashMap<String, Vec<String>> = HashMap::new();
    for (key, value) in req.queries().iter() {
        queries.entry(key.clone()).or_default().push(value.clone());
    }

    let mut params: HashMap<String, String> = HashMap::new();
    for (key, value) in req.params().iter() {
        params.insert(key.clone(), value.clone());
    }

    // A body that cannot be read is an empty body here; actions that require
    // one surface that themselves via `body_json`, with a better message.
    let body = req.payload().await.ok().cloned().unwrap_or_default();

    HttpContext::new(
        Request::new(method, uri, headers, queries, body),
        Params::new(params),
        services,
        config,
    )
}

pub fn write_response(out: &mut salvo::Response, response: Response) {
    out.status_code(response.status);

    for (name, value) in &response.headers {
        // Header names/values are app-supplied strings, so both conversions
        // are fallible; a malformed header is dropped rather than fatal.
        if let (Ok(name), Ok(value)) = (
            salvo::http::HeaderName::try_from(name.as_str()),
            salvo::http::HeaderValue::try_from(value.as_str()),
        ) {
            out.headers_mut().append(name, value);
        }
    }

    match response.body {
        Body::Empty => {}
        Body::Json(value) => {
            let payload = serde_json::to_vec(&value)
                .unwrap_or_else(|_| br#"{"title":"internal server error","status":500}"#.to_vec());
            let _ = out.add_header("content-type", "application/json; charset=utf-8", true);
            let _ = out.write_body(payload);
        }
        Body::Bytes { data, content_type } => {
            let _ = out.add_header("content-type", content_type, true);
            let _ = out.write_body(data);
        }
    }
}

pub fn write_error(out: &mut salvo::Response, error: Error) {
    // Internal failures are logged in full and redacted in the response body.
    if let Error::Internal(inner) = &error {
        eprintln!("luxid: unhandled error: {inner:?}");
    }

    out.status_code(error.status_code());

    let payload = serde_json::to_vec(&error.problem())
        .unwrap_or_else(|_| br#"{"title":"internal server error","status":500}"#.to_vec());

    let _ = out.add_header(
        "content-type",
        "application/problem+json; charset=utf-8",
        true,
    );
    let _ = out.write_body(payload);
}

/// One salvo handler per route, owning that route's resolved middleware stack.
///
/// This is the single seam between Luxid and salvo: the context is built here,
/// threaded through the chain, and the outcome written back.
pub(crate) struct RouteHandler {
    stack: Arc<[Arc<dyn Middleware>]>,
    action: Arc<dyn Action>,
    services: Container,
    config: Config,
}

impl RouteHandler {
    pub(crate) fn new(
        stack: Vec<Arc<dyn Middleware>>,
        action: Arc<dyn Action>,
        services: Container,
        config: Config,
    ) -> Self {
        Self {
            stack: stack.into(),
            action,
            services,
            config,
        }
    }
}

#[salvo::async_trait]
impl salvo::Handler for RouteHandler {
    async fn handle(
        &self,
        req: &mut salvo::Request,
        depot: &mut salvo::Depot,
        res: &mut salvo::Response,
        ctrl: &mut salvo::FlowCtrl,
    ) {
        let _ = (&depot, &ctrl);

        // A fresh scope per request: singletons are shared, scoped services
        // are not.
        let context = build_context(req, self.services.scope(), self.config.clone()).await;
        let next = Next::new(self.stack.clone(), self.action.clone());

        match next.run(context).await {
            Ok(response) => write_response(res, response),
            Err(error) => write_error(res, error),
        }
    }
}
