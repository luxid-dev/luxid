//! Authentication observed through real requests: issuing a token, guarding a
//! route, and reading the identity in an action.

use std::time::Duration;

use luxid::__private::salvo::test::{ResponseExt, TestClient};
use luxid::prelude::*;
use serde_json::{Value, json};

const SECRET: &str = "test-secret-value";

/// Stands in for a users table until the Lucid layer lands.
fn find_user(email: &str) -> Option<(i64, &'static str, &'static str)> {
    match email {
        "ada@example.com" => Some((1, "admin", "$ada")),
        "alan@example.com" => Some((2, "member", "$alan")),
        _ => None,
    }
}

pub struct AuthController;

#[luxid::controller]
impl AuthController {
    async fn login(ctx: HttpContext) -> Result<Response> {
        let body: Value = ctx.request.body_json()?;
        let email = body
            .get("email")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let password = body
            .get("password")
            .and_then(Value::as_str)
            .unwrap_or_default();

        // A wrong email and a wrong password are indistinguishable to a caller.
        let Some((id, role, stored)) = find_user(email) else {
            return Err(Error::Unauthorized);
        };
        if password != stored.trim_start_matches('$') {
            return Err(Error::Unauthorized);
        }

        let jwt = ctx.services.get::<Jwt>()?;
        let identity = Identity::new(id.to_string()).with_claim("role", role);

        ctx.response.ok(json!({ "token": jwt.sign(&identity)? }))
    }
}

pub struct MeController;

#[luxid::controller]
impl MeController {
    async fn show(ctx: HttpContext) -> Result<Response> {
        let id: i64 = ctx.auth.id()?;
        let role: Option<String> = ctx.auth.identity()?.claim("role")?;

        ctx.response.ok(json!({ "id": id, "role": role }))
    }

    /// Behind the optional guard: renders either way.
    async fn feed(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({
            "authenticated": ctx.auth.check(),
            "id": ctx.auth.try_identity().map(|identity| identity.subject().to_owned()),
        }))
    }
}

fn service_with(jwt: Jwt) -> luxid::__private::salvo::Service {
    App::new()
        .providers(Providers::new().singleton(move |_| Jwt::new(SECRET).with_ttl(jwt.ttl())))
        .routes(|r| {
            r.post("/login", AuthController::login);
            r.get("/me", MeController::show).middleware(Auth::jwt());
            r.get("/feed", MeController::feed)
                .middleware(Auth::optional_jwt());
        })
        .into_service()
}

fn service() -> luxid::__private::salvo::Service {
    service_with(Jwt::new(SECRET))
}

const BASE: &str = "http://127.0.0.1:5800";

async fn login(service: &luxid::__private::salvo::Service, email: &str, password: &str) -> String {
    let mut res = TestClient::post(format!("{BASE}/login"))
        .json(&json!({ "email": email, "password": password }))
        .send(service)
        .await;

    let body: Value = res.take_json().await.expect("json body");
    body["token"].as_str().expect("a token").to_owned()
}

#[tokio::test]
async fn a_valid_login_issues_a_usable_token() {
    let service = service();
    let token = login(&service, "ada@example.com", "ada").await;

    let mut res = TestClient::get(format!("{BASE}/me"))
        .add_header("authorization", format!("Bearer {token}"), true)
        .send(&service)
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(200));

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["id"], 1);
    assert_eq!(body["role"], "admin");
}

#[tokio::test]
async fn identities_do_not_bleed_between_users() {
    let service = service();
    let token = login(&service, "alan@example.com", "alan").await;

    let mut res = TestClient::get(format!("{BASE}/me"))
        .add_header("authorization", format!("Bearer {token}"), true)
        .send(&service)
        .await;

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["id"], 2);
    assert_eq!(body["role"], "member");
}

#[tokio::test]
async fn a_bad_password_and_an_unknown_email_are_indistinguishable() {
    let service = service();

    let mut wrong_password = TestClient::post(format!("{BASE}/login"))
        .json(&json!({ "email": "ada@example.com", "password": "nope" }))
        .send(&service)
        .await;

    let mut unknown_email = TestClient::post(format!("{BASE}/login"))
        .json(&json!({ "email": "nobody@example.com", "password": "nope" }))
        .send(&service)
        .await;

    assert_eq!(wrong_password.status_code, unknown_email.status_code);
    assert_eq!(
        wrong_password.take_json::<Value>().await.expect("json"),
        unknown_email.take_json::<Value>().await.expect("json")
    );
}

#[tokio::test]
async fn a_guarded_route_rejects_a_missing_token() {
    let mut res = TestClient::get(format!("{BASE}/me")).send(&service()).await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(401));

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["type"], "https://luxid.rs/errors/unauthorized");
}

#[tokio::test]
async fn a_guarded_route_rejects_a_forged_token() {
    let forged = Jwt::new("a-different-secret")
        .sign(&Identity::new("1"))
        .expect("signs");

    let res = TestClient::get(format!("{BASE}/me"))
        .add_header("authorization", format!("Bearer {forged}"), true)
        .send(&service())
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(401));
}

#[tokio::test]
async fn an_expired_token_is_rejected_by_the_guard() {
    let service = service_with(Jwt::new(SECRET).with_ttl(Duration::from_secs(1)));

    // Issued in the past rather than sleeping, so the test stays fast.
    let expired = Jwt::new(SECRET)
        .sign_expiring_at(&Identity::new("1"), 1_000_000)
        .expect("signs");

    let res = TestClient::get(format!("{BASE}/me"))
        .add_header("authorization", format!("Bearer {expired}"), true)
        .send(&service)
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(401));
}

#[tokio::test]
async fn the_optional_guard_lets_anonymous_requests_through() {
    let mut res = TestClient::get(format!("{BASE}/feed"))
        .send(&service())
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(200));

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["authenticated"], false);
    assert_eq!(body["id"], Value::Null);
}

#[tokio::test]
async fn the_optional_guard_still_resolves_a_present_token() {
    let service = service();
    let token = login(&service, "ada@example.com", "ada").await;

    let mut res = TestClient::get(format!("{BASE}/feed"))
        .add_header("authorization", format!("Bearer {token}"), true)
        .send(&service)
        .await;

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["authenticated"], true);
    assert_eq!(body["id"], "1");
}

#[tokio::test]
async fn a_guard_without_a_configured_jwt_is_a_redacted_500() {
    // The Jwt singleton was never registered.
    let service = App::new()
        .routes(|r| {
            r.get("/me", MeController::show).middleware(Auth::jwt());
        })
        .into_service();

    let mut res = TestClient::get(format!("{BASE}/me"))
        .add_header("authorization", "Bearer anything", true)
        .send(&service)
        .await;

    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(500));

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["title"], "internal server error");
    assert!(!body.to_string().contains("Jwt"));
}
