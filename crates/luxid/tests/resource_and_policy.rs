//! `r.resource(..)` and `ctx.authorize(..)`.

use luxid::prelude::*;
use luxid_testing::TestApp;
use serde_json::json;

/// Stands in for a persisted row.
#[derive(Debug, Clone)]
struct Post {
    id: i64,
    owner: i64,
}

/// A policy is an ordinary function of `(&Auth, &T) -> bool`.
struct PostPolicy;

impl PostPolicy {
    fn update(auth: &Auth, post: &Post) -> bool {
        auth.try_identity()
            .and_then(|identity| identity.id::<i64>().ok())
            .is_some_and(|id| id == post.owner)
    }

    fn view(_auth: &Auth, _post: &Post) -> bool {
        true
    }
}

fn post(id: i64) -> Post {
    Post { id, owner: 1 }
}

pub struct PostsController;

#[luxid::controller]
impl PostsController {
    async fn index(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({ "action": "index" }))
    }

    async fn store(ctx: HttpContext) -> Result<Response> {
        ctx.response.created(json!({ "action": "store" }))
    }

    async fn show(ctx: HttpContext) -> Result<Response> {
        let id: i64 = ctx.params.get("id")?;
        let post = post(id);

        // Bind first: `ctx.response.ok(..)` moves the response before its
        // argument is evaluated, so the argument cannot also borrow `ctx`.
        let can_update = ctx.can(PostPolicy::update, &post);

        ctx.response.ok(json!({
            "action": "show",
            "id": post.id,
            "can_update": can_update,
        }))
    }

    async fn update(ctx: HttpContext) -> Result<Response> {
        let id: i64 = ctx.params.get("id")?;
        let post = post(id);

        // Denial is a 403 through the ordinary error path.
        ctx.authorize(PostPolicy::update, &post)?;

        ctx.response
            .ok(json!({ "action": "update", "id": post.id }))
    }

    async fn destroy(ctx: HttpContext) -> Result<Response> {
        ctx.authorize(PostPolicy::update, &post(ctx.params.get("id")?))?;
        ctx.response.no_content()
    }

    /// Not a resource action; must not become a route.
    async fn archive(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({ "action": "archive" }))
    }
}

/// A read-only controller: `resource` must register two routes, not five.
pub struct ReportsController;

#[luxid::controller]
impl ReportsController {
    async fn index(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({ "action": "index" }))
    }

    async fn show(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({ "action": "show" }))
    }
}

const SECRET: &str = "resource-test-secret";

fn build() -> App {
    App::new()
        .providers(Providers::new().singleton(|_| Jwt::new(SECRET)))
        .routes(|r| {
            r.group("/api", |r| {
                r.resource("/posts", PostsController)
                    .middleware(Auth::optional_jwt());
                r.resource("/reports", ReportsController);
            });
        })
}

fn token(subject: &str) -> String {
    Jwt::new(SECRET)
        .sign(&Identity::new(subject))
        .expect("signs")
}

fn app() -> TestApp {
    TestApp::new(build().into_service())
}

#[test]
fn resource_registers_exactly_the_actions_that_exist() {
    let table = build().route_table();

    let rows: Vec<(String, String, &str)> = table
        .iter()
        .map(|route| {
            (
                route.method.as_str().to_owned(),
                route.path.clone(),
                route.action,
            )
        })
        .collect();

    assert_eq!(
        rows,
        vec![
            ("GET".into(), "/api/posts".into(), "PostsController::index"),
            ("POST".into(), "/api/posts".into(), "PostsController::store"),
            (
                "GET".into(),
                "/api/posts/{id}".into(),
                "PostsController::show"
            ),
            (
                "PUT".into(),
                "/api/posts/{id}".into(),
                "PostsController::update"
            ),
            (
                "DELETE".into(),
                "/api/posts/{id}".into(),
                "PostsController::destroy"
            ),
            // Two, not five: ReportsController defines only index and show.
            (
                "GET".into(),
                "/api/reports".into(),
                "ReportsController::index"
            ),
            (
                "GET".into(),
                "/api/reports/{id}".into(),
                "ReportsController::show"
            ),
        ]
    );
}

#[test]
fn the_collection_route_has_no_trailing_slash() {
    let table = build().route_table();
    assert!(
        table.iter().all(|route| !route.path.ends_with('/')),
        "{table:#?}"
    );
}

#[test]
fn middleware_attaches_to_the_whole_resource() {
    let table = build().route_table();

    for route in table
        .iter()
        .filter(|route| route.path.starts_with("/api/posts"))
    {
        assert_eq!(route.middleware, 1, "{} lost its guard", route.path);
    }
    for route in table
        .iter()
        .filter(|route| route.path.starts_with("/api/reports"))
    {
        assert_eq!(route.middleware, 0);
    }
}

#[tokio::test]
async fn a_non_resource_action_gets_no_route_of_its_own() {
    assert!(
        build()
            .route_table()
            .iter()
            .all(|route| route.action != "PostsController::archive"),
        "archive is not one of the five and must not be registered"
    );

    // The request falls through to `show`, whose `{id}` cannot read "archive" —
    // a 400, not a dispatch to `archive`. Any framework with `/posts/{id}`
    // behaves this way.
    app()
        .get("/api/posts/archive")
        .send()
        .await
        .assert_status(400);
}

#[tokio::test]
async fn resource_routes_answer() {
    let app = app();

    app.get("/api/posts")
        .send()
        .await
        .assert_ok()
        .assert_json_path("action", "index");
    app.post("/api/posts").send().await.assert_created();
    app.get("/api/posts/7")
        .send()
        .await
        .assert_ok()
        .assert_json_path("id", 7);
    app.get("/api/reports").send().await.assert_ok();
}

#[tokio::test]
async fn a_policy_permits_the_owner() {
    app()
        .put("/api/posts/7")
        .bearer(token("1"))
        .send()
        .await
        .assert_ok()
        .assert_json_path("action", "update");
}

#[tokio::test]
async fn a_policy_denies_everyone_else_with_a_403() {
    app()
        .put("/api/posts/7")
        .bearer(token("2"))
        .send()
        .await
        .assert_forbidden()
        .assert_json_path("type", "https://luxid.rs/errors/forbidden");
}

#[tokio::test]
async fn an_anonymous_request_is_denied_too() {
    app().delete("/api/posts/7").send().await.assert_forbidden();
}

#[tokio::test]
async fn can_answers_without_denying() {
    // `can` is for deciding what to render, so it must not fail the request.
    app()
        .get("/api/posts/7")
        .bearer(token("2"))
        .send()
        .await
        .assert_ok()
        .assert_json_path("can_update", false);

    app()
        .get("/api/posts/7")
        .bearer(token("1"))
        .send()
        .await
        .assert_ok()
        .assert_json_path("can_update", true);
}

#[test]
fn a_permissive_policy_allows() {
    // Guards the shape of the policy signature itself.
    let auth = Auth::default();
    assert!(PostPolicy::view(&auth, &post(1)));
    assert!(!PostPolicy::update(&auth, &post(1)));
}
