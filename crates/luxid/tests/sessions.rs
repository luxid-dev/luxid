//! Cookie-backed sessions across real requests.

use std::sync::Arc;

use luxid::prelude::*;
use luxid_testing::TestApp;
use serde_json::json;

pub struct SessionController;

#[luxid::controller]
impl SessionController {
    /// Who am I, according to the session?
    async fn me(ctx: HttpContext) -> Result<Response> {
        let visits: u32 = ctx.session.get("visits")?.unwrap_or(0);
        ctx.session.put("visits", visits + 1)?;

        let subject = ctx.auth.try_identity().map(|i| i.subject().to_owned());

        ctx.response.ok(json!({
            "authenticated": ctx.auth.check(),
            "subject": subject,
            "visits": visits,
        }))
    }

    async fn login(ctx: HttpContext) -> Result<Response> {
        ctx.session.login(&Identity::new("42"))?;
        ctx.response.ok(json!({ "ok": true }))
    }

    async fn logout(ctx: HttpContext) -> Result<Response> {
        ctx.session.logout()?;
        ctx.response.ok(json!({ "ok": true }))
    }

    /// Outside any session middleware.
    async fn detached(ctx: HttpContext) -> Result<Response> {
        ctx.session.put("nope", 1)?;
        ctx.response.ok(json!({ "unreachable": true }))
    }
}

fn app() -> TestApp {
    TestApp::new(
        App::new()
            .providers(
                Providers::new().bind::<dyn SessionStore, _>(|_| Arc::new(MemoryStore::new())),
            )
            .routes(|r| {
                r.group("/s", |r| {
                    r.middleware(Auth::session());

                    r.get("/me", SessionController::me);
                    r.post("/login", SessionController::login);
                    r.post("/logout", SessionController::logout);
                });

                r.get("/detached", SessionController::detached);
            })
            .into_service(),
    )
}

/// The session id from a `set-cookie` header, if one was issued.
fn issued_cookie(response: &luxid_testing::TestResponse) -> Option<String> {
    let header = response.header("set-cookie")?;
    let (pair, _) = header.split_once(';')?;
    let (_, value) = pair.split_once('=')?;

    Some(value.to_owned())
}

#[tokio::test]
async fn a_first_request_is_issued_a_session_cookie() {
    let response = app().get("/s/me").send().await.assert_ok();

    let cookie = issued_cookie(&response).expect("a session cookie was issued");
    assert_eq!(cookie.len(), 64, "32 random bytes as hex");

    let header = response.header("set-cookie").expect("header");
    assert!(header.contains("HttpOnly"), "{header}");
    assert!(header.contains("SameSite=Lax"), "{header}");
    assert!(header.contains("Path=/"), "{header}");
}

#[tokio::test]
async fn session_values_persist_across_requests() {
    let app = app();

    let first = app
        .get("/s/me")
        .send()
        .await
        .assert_ok()
        .assert_json_path("visits", 0);
    let cookie = issued_cookie(&first).expect("cookie");

    app.get("/s/me")
        .header("cookie", format!("luxid_session={cookie}"))
        .send()
        .await
        .assert_ok()
        .assert_json_path("visits", 1);

    app.get("/s/me")
        .header("cookie", format!("luxid_session={cookie}"))
        .send()
        .await
        .assert_json_path("visits", 2);
}

#[tokio::test]
async fn an_unknown_cookie_starts_a_fresh_session_rather_than_failing() {
    // A stale cookie is ordinary — a restarted store, an expired entry.
    app()
        .get("/s/me")
        .header("cookie", "luxid_session=deadbeef")
        .send()
        .await
        .assert_ok()
        .assert_json_path("visits", 0)
        .assert_json_path("authenticated", false);
}

#[tokio::test]
async fn logging_in_authenticates_later_requests() {
    let app = app();

    let login = app.post("/s/login").send().await.assert_ok();
    let cookie = issued_cookie(&login).expect("login issues a cookie");

    app.get("/s/me")
        .header("cookie", format!("luxid_session={cookie}"))
        .send()
        .await
        .assert_ok()
        .assert_json_path("authenticated", true)
        .assert_json_path("subject", "42");
}

#[tokio::test]
async fn login_rotates_the_id_so_a_fixed_cookie_is_useless() {
    let app = app();

    // An attacker fixes a session id before the victim logs in.
    let planted = app.get("/s/me").send().await;
    let before = issued_cookie(&planted).expect("cookie");

    let login = app
        .post("/s/login")
        .header("cookie", format!("luxid_session={before}"))
        .send()
        .await
        .assert_ok();

    let after = issued_cookie(&login).expect("login re-issues the cookie");
    assert_ne!(after, before, "the id must rotate on login");

    // The planted id is now worthless.
    app.get("/s/me")
        .header("cookie", format!("luxid_session={before}"))
        .send()
        .await
        .assert_json_path("authenticated", false);

    app.get("/s/me")
        .header("cookie", format!("luxid_session={after}"))
        .send()
        .await
        .assert_json_path("authenticated", true);
}

#[tokio::test]
async fn logging_out_invalidates_the_session_and_clears_the_cookie() {
    let app = app();

    let login = app.post("/s/login").send().await;
    let cookie = issued_cookie(&login).expect("cookie");

    let logout = app
        .post("/s/logout")
        .header("cookie", format!("luxid_session={cookie}"))
        .send()
        .await
        .assert_ok();

    let header = logout.header("set-cookie").expect("a removal cookie");
    assert!(header.contains("Max-Age=0"), "{header}");

    // The old cookie no longer authenticates.
    app.get("/s/me")
        .header("cookie", format!("luxid_session={cookie}"))
        .send()
        .await
        .assert_json_path("authenticated", false);
}

#[tokio::test]
async fn sessions_do_not_bleed_between_clients() {
    let app = app();

    let a = issued_cookie(&app.post("/s/login").send().await).expect("cookie");
    let b = issued_cookie(&app.get("/s/me").send().await).expect("cookie");

    assert_ne!(a, b);

    app.get("/s/me")
        .header("cookie", format!("luxid_session={b}"))
        .send()
        .await
        .assert_json_path("authenticated", false);
}

#[tokio::test]
async fn writing_a_session_without_the_middleware_says_so() {
    app()
        .get("/detached")
        .send()
        .await
        .assert_status(500)
        .assert_json_path("title", "internal server error");
}

#[tokio::test]
async fn a_route_with_sessions_but_no_store_reports_the_missing_binding() {
    let app = TestApp::new(
        App::new()
            .routes(|r| {
                r.get("/s/me", SessionController::me)
                    .middleware(Auth::session());
            })
            .into_service(),
    );

    app.get("/s/me").send().await.assert_status(500);
}
