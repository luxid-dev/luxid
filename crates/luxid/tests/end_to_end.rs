//! End-to-end coverage of the request path: `#[luxid::controller]` → router →
//! salvo adapter → response. These exercise the same code production runs.

use luxid::__private::salvo::test::{ResponseExt, TestClient};
use luxid::ValidationErrors;
use luxid::prelude::*;
use serde_json::{Value, json};

pub struct UsersController;

#[luxid::controller]
impl UsersController {
    /// Whole-context style — the house default.
    async fn index(ctx: HttpContext) -> Result<Response> {
        let page = ctx.request.input::<u32>("page")?.unwrap_or(1);
        ctx.response.ok(json!({ "page": page, "data": [] }))
    }

    /// Destructured style — same signature type, purely a style choice.
    async fn store(
        HttpContext {
            request, response, ..
        }: HttpContext,
    ) -> Result<Response> {
        let body: Value = request.body_json()?;
        let name = body.get("name").and_then(Value::as_str).unwrap_or_default();

        if name.is_empty() {
            let mut errors = ValidationErrors::new();
            errors.add("name", "is required");
            return Err(Error::Validation(errors));
        }

        response.created(json!({ "id": 1, "name": name }))
    }

    async fn show(ctx: HttpContext) -> Result<Response> {
        let id: i64 = ctx.params.get("id")?;

        if id != 1 {
            return Err(Error::not_found("User", id));
        }

        ctx.response
            .header("x-source", "luxid")
            .ok(json!({ "id": id }))
    }

    async fn destroy(ctx: HttpContext) -> Result<Response> {
        ctx.response.no_content()
    }

    /// Not an action (takes no context) — must be left alone by the macro.
    fn helper_untouched() -> u8 {
        42
    }
}

fn service() -> luxid::__private::salvo::Service {
    App::new()
        .routes(|r| {
            r.group("/api/v1", |r| {
                r.get("/users", UsersController::index);
                r.post("/users", UsersController::store);
                r.get("/users/{id}", UsersController::show);
                r.delete("/users/{id}", UsersController::destroy);
            });
        })
        .into_service()
}

const BASE: &str = "http://127.0.0.1:5800/api/v1";

#[tokio::test]
async fn whole_context_action_reads_query_input() {
    let mut res = TestClient::get(format!("{BASE}/users?page=3"))
        .send(&service())
        .await;

    assert_eq!(
        res.status_code,
        Some(luxid::__private::salvo::http::StatusCode::OK)
    );
    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["page"], 3);
}

#[tokio::test]
async fn query_input_falls_back_to_a_default() {
    let mut res = TestClient::get(format!("{BASE}/users"))
        .send(&service())
        .await;

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["page"], 1);
}

#[tokio::test]
async fn destructured_action_returns_201() {
    let mut res = TestClient::post(format!("{BASE}/users"))
        .json(&json!({ "name": "Ada" }))
        .send(&service())
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(201));
    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["name"], "Ada");
}

#[tokio::test]
async fn validation_failures_render_rfc7807() {
    let mut res = TestClient::post(format!("{BASE}/users"))
        .json(&json!({ "name": "" }))
        .send(&service())
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(422));
    assert_eq!(
        res.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json; charset=utf-8")
    );

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["status"], 422);
    assert_eq!(body["type"], "https://luxid.rs/errors/validation");
    assert_eq!(body["errors"]["name"][0], "is required");
}

#[tokio::test]
async fn route_params_decode_into_typed_values() {
    let mut res = TestClient::get(format!("{BASE}/users/1"))
        .send(&service())
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(200));
    assert_eq!(
        res.headers().get("x-source").and_then(|v| v.to_str().ok()),
        Some("luxid")
    );

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["id"], 1);
}

#[tokio::test]
async fn not_found_maps_to_404_with_a_problem_body() {
    let mut res = TestClient::get(format!("{BASE}/users/99"))
        .send(&service())
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(404));

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["status"], 404);
    assert_eq!(body["resource"], "User");
    assert_eq!(body["id"], "99");
}

#[tokio::test]
async fn no_content_returns_204_with_an_empty_body() {
    let mut res = TestClient::delete(format!("{BASE}/users/1"))
        .send(&service())
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(204));
    assert_eq!(res.take_string().await.expect("body").trim(), "");
}

#[tokio::test]
async fn unrouted_paths_are_not_claimed_by_the_group() {
    let res = TestClient::get("http://127.0.0.1:5800/nope")
        .send(&service())
        .await;
    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(404));
}

#[test]
fn non_action_items_survive_the_macro() {
    assert_eq!(UsersController::helper_untouched(), 42);
}
