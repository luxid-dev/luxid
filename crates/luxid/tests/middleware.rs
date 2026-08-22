//! Middleware behaviour, exercised through the full request path.
//!
//! Ordering and short-circuiting are asserted over real responses rather than
//! by inspecting the router, so these prove what a user would actually observe.

use luxid::__private::salvo::test::{ResponseExt, TestClient};
use luxid::prelude::*;
use serde_json::{Value, json};

/// Set by `Authenticate`, read by the action. This is the hand-off an owning
/// context makes possible: one context is built per request and threaded
/// through the chain, so a mutation here is visible downstream.
#[derive(Debug, Clone, PartialEq)]
struct CurrentUser {
    id: i64,
}

/// Appends its label to a response header on the way out, so the assembled
/// header records the exact order the chain unwound.
struct Trace(&'static str);

#[luxid::middleware]
impl Trace {
    async fn handle(&self, ctx: HttpContext, next: Next) -> Result<Response> {
        let response = next.run(ctx).await?;
        Ok(response.header("x-trace", self.0))
    }
}

struct Authenticate;

#[luxid::middleware]
impl Authenticate {
    async fn handle(&self, mut ctx: HttpContext, next: Next) -> Result<Response> {
        let token = ctx.request.bearer_token().ok_or(Error::Unauthorized)?;

        let id: i64 = token.parse().map_err(|_| Error::Unauthorized)?;
        ctx.extensions.insert(CurrentUser { id });

        next.run(ctx).await
    }
}

/// Rejects before the action ever runs.
struct Gate;

#[luxid::middleware]
impl Gate {
    async fn handle(&self, ctx: HttpContext, _next: Next) -> Result<Response> {
        let _ = ctx;
        Err(Error::Forbidden)
    }
}

pub struct MeController;

#[luxid::controller]
impl MeController {
    async fn show(ctx: HttpContext) -> Result<Response> {
        let user = ctx
            .extensions
            .get::<CurrentUser>()
            .ok_or_else(|| Error::internal("Authenticate did not run"))?
            .clone();

        ctx.response.ok(json!({ "id": user.id }))
    }

    async fn ping(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({ "pong": true }))
    }

    async fn blocked(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({ "reached": true }))
    }
}

fn service() -> luxid::__private::salvo::Service {
    App::new()
        .middleware(Trace("global"))
        .routes(|r| {
            r.group("/api", |r| {
                r.middleware(Trace("group"));

                r.get("/me", MeController::show).middleware(Authenticate);
                r.get("/ping", MeController::ping)
                    .middleware(Trace("route"));
                r.get("/blocked", MeController::blocked).middleware(Gate);
            });
        })
        .into_service()
}

const BASE: &str = "http://127.0.0.1:5800/api";

fn header(res: &luxid::__private::salvo::Response, name: &str) -> Vec<String> {
    res.headers()
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn middleware_runs_outermost_first_and_unwinds_inside_out() {
    let res = TestClient::get(format!("{BASE}/ping"))
        .send(&service())
        .await;

    // Entered global → group → route; unwound route → group → global.
    assert_eq!(header(&res, "x-trace"), vec!["route", "group", "global"]);
}

#[tokio::test]
async fn middleware_hands_values_to_the_action_through_the_context() {
    let mut res = TestClient::get(format!("{BASE}/me"))
        .add_header("authorization", "Bearer 77", true)
        .send(&service())
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(200));

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["id"], 77);
}

#[tokio::test]
async fn a_rejecting_middleware_short_circuits_the_action() {
    let mut res = TestClient::get(format!("{BASE}/me")).send(&service()).await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(401));

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["type"], "https://luxid.rs/errors/unauthorized");
}

#[tokio::test]
async fn an_error_skips_the_after_hooks_of_outer_middleware() {
    let res = TestClient::get(format!("{BASE}/blocked"))
        .send(&service())
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(403));
    // Gate returned Err, so the `?` in each enclosing Trace propagated it
    // without reaching the header line: after-hooks are skipped on the error
    // path, exactly as `?` in ordinary code would behave.
    assert!(header(&res, "x-trace").is_empty());
}

#[tokio::test]
async fn route_middleware_does_not_leak_to_sibling_routes() {
    let res = TestClient::get(format!("{BASE}/me"))
        .add_header("authorization", "Bearer 5", true)
        .send(&service())
        .await;

    // `/me` carries Authenticate, not Trace("route").
    assert_eq!(header(&res, "x-trace"), vec!["group", "global"]);
}
