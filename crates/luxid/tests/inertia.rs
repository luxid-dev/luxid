//! The Inertia.js protocol: shell vs JSON, partial reloads, asset versioning,
//! and the redirect-back-with-errors flow that shapes the whole design.

use std::sync::Arc;

use luxid::prelude::*;
use luxid_testing::{TestApp, TestResponse};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize, Validate, luxid::JsonSchema)]
struct StorePost {
    #[validate(length(min = 3))]
    title: String,
}

pub struct PagesController;

#[luxid::controller]
impl PagesController {
    async fn home(ctx: HttpContext) -> Result<Response> {
        ctx.inertia("Home", json!({ "message": "hello", "extra": 1 }))
    }

    /// Fails validation for a short title, which is the interesting path.
    async fn store(ctx: HttpContext) -> Result<Response> {
        let input = ctx.request.validate::<StorePost>().await?;

        ctx.inertia("Posts/Show", json!({ "title": input.title }))
    }

    /// Outside the Inertia middleware.
    async fn detached(ctx: HttpContext) -> Result<Response> {
        ctx.inertia("Nope", json!({}))
    }
}

fn app() -> TestApp {
    build(Inertia::new("resources/js/app.jsx").version("v1"))
}

fn build(inertia: Inertia) -> TestApp {
    TestApp::new(
        App::new()
            .providers(
                Providers::new().bind::<dyn SessionStore, _>(|_| Arc::new(MemoryStore::new())),
            )
            .routes(move |r| {
                r.group("/", |r| {
                    r.middleware(Auth::session());
                    r.middleware(inertia);

                    r.get("/", PagesController::home);
                    r.post("/posts", PagesController::store);
                });

                r.get("/detached", PagesController::detached);
            })
            .into_service(),
    )
}

fn cookie(response: &TestResponse) -> Option<String> {
    let header = response.header("set-cookie")?;
    let (pair, _) = header.split_once(';')?;
    let (_, value) = pair.split_once('=')?;
    Some(value.to_owned())
}

// ---- shell vs JSON --------------------------------------------------------

#[tokio::test]
async fn a_plain_browser_load_gets_an_html_shell() {
    let response = app().get("/").send().await.assert_ok();

    assert_eq!(
        response.header("content-type"),
        Some("text/html; charset=utf-8")
    );

    let body = response.body();
    assert!(body.contains("<div id=\"app\" data-page="), "{body}");
    // The page object is embedded, HTML-escaped, in the attribute.
    assert!(
        body.contains("&quot;component&quot;:&quot;Home&quot;"),
        "{body}"
    );
    // Debug builds point at the Vite dev server.
    assert!(
        body.contains("http://localhost:5173/@vite/client"),
        "{body}"
    );
}

#[tokio::test]
async fn an_inertia_request_gets_json() {
    let response = app()
        .get("/")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .send()
        .await
        .assert_ok()
        .assert_header("x-inertia", "true")
        // Without Vary, a cache could hand this JSON to a browser navigation.
        .assert_header("vary", "X-Inertia");

    let page = response.json();
    assert_eq!(page["component"], "Home");
    assert_eq!(page["props"]["message"], "hello");
    assert_eq!(page["url"], "/");
    assert_eq!(page["version"], "v1");
}

#[tokio::test]
async fn the_url_carries_the_query_string() {
    let response = app()
        .get("/?page=2")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .send()
        .await
        .assert_ok();

    assert_eq!(response.json()["url"], "/?page=2");
}

#[tokio::test]
async fn a_route_without_the_middleware_says_so() {
    // A silently wrong render would be much worse than a loud failure.
    let response = app().get("/detached").send().await.assert_status(500);

    assert!(!response.body().contains("Nope"));
}

// ---- asset versioning -----------------------------------------------------

#[tokio::test]
async fn a_stale_asset_version_forces_a_reload() {
    let response = app()
        .get("/")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v0")
        .send()
        .await
        .assert_status(409);

    // The client abandons the XHR and does a full browser load of this URL,
    // which is how a deploy reaches tabs that are already open.
    assert_eq!(response.header("x-inertia-location"), Some("/"));
}

#[tokio::test]
async fn a_matching_version_is_not_disturbed() {
    app()
        .get("/")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .send()
        .await
        .assert_ok();
}

// ---- partial reloads ------------------------------------------------------

#[tokio::test]
async fn a_partial_reload_returns_only_the_requested_props() {
    let response = app()
        .get("/")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .header("x-inertia-partial-component", "Home")
        .header("x-inertia-partial-data", "message")
        .send()
        .await
        .assert_ok();

    let props = &response.json()["props"];
    assert_eq!(props["message"], "hello");
    assert!(props.get("extra").is_none(), "extra should be filtered out");
}

#[tokio::test]
async fn a_partial_for_a_different_component_is_ignored() {
    // Honouring it would deliver a page missing most of its data.
    let response = app()
        .get("/")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .header("x-inertia-partial-component", "SomethingElse")
        .header("x-inertia-partial-data", "message")
        .send()
        .await
        .assert_ok();

    assert_eq!(response.json()["props"]["extra"], 1);
}

// ---- validation: the redirect-back flow -----------------------------------

#[tokio::test]
async fn a_validation_failure_redirects_back_with_errors_flashed() {
    let app = app();

    // Visit the form page first: that is what records where "back" is.
    let visit = app
        .get("/")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .send()
        .await
        .assert_ok();

    let session = cookie(&visit).expect("a session cookie");

    // Submit something invalid.
    let rejected = app
        .post("/posts")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .with_session(&session)
        .json(json!({ "title": "no" }))
        .send()
        .await;

    // 303, not 422 and not 302: a 302 after a PUT/DELETE would have the browser
    // repeat the method against the new URL.
    assert_eq!(rejected.status(), 303);
    assert_eq!(rejected.header("location"), Some("/"));

    // The next page load carries the errors as a shared prop.
    let back = app
        .get("/")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .with_session(&session)
        .send()
        .await
        .assert_ok();

    assert_eq!(
        back.json()["props"]["errors"]["title"],
        "must be at least 3 characters"
    );
}

#[tokio::test]
async fn flashed_errors_survive_exactly_one_request() {
    let app = app();

    let visit = app
        .get("/")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .send()
        .await;
    let session = cookie(&visit).expect("a session cookie");

    app.post("/posts")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .with_session(&session)
        .json(json!({ "title": "no" }))
        .send()
        .await;

    // First read: present.
    let first = app
        .get("/")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .with_session(&session)
        .send()
        .await;
    assert_eq!(
        first.json()["props"]["errors"]["title"],
        "must be at least 3 characters"
    );

    // Second read: gone. Otherwise a stale error renders on an unrelated page.
    let second = app
        .get("/")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .with_session(&session)
        .send()
        .await;
    assert_eq!(second.json()["props"]["errors"], json!({}));
}

#[tokio::test]
async fn a_valid_submission_is_not_redirected() {
    let app = app();
    let visit = app
        .get("/")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .send()
        .await;
    let session = cookie(&visit).expect("a session cookie");

    let response = app
        .post("/posts")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .with_session(&session)
        .json(json!({ "title": "long enough" }))
        .send()
        .await
        .assert_ok();

    assert_eq!(response.json()["component"], "Posts/Show");
}

#[tokio::test]
async fn a_non_inertia_request_still_gets_the_422() {
    // The whole point of doing this in middleware: `Error` is untouched, so a
    // JSON API client hitting the same action sees the ordinary problem
    // document. Same validator, same action, two renderings.
    let app = app();

    let response = app
        .post("/posts")
        .json(json!({ "title": "no" }))
        .send()
        .await
        .assert_status(422);

    assert_eq!(
        response.header("content-type"),
        Some("application/problem+json; charset=utf-8")
    );
    assert_eq!(
        response.json()["errors"]["title"][0],
        "must be at least 3 characters"
    );
}

// ---- shared props ---------------------------------------------------------

#[tokio::test]
async fn shared_props_are_merged_into_every_page() {
    let app = build(
        Inertia::new("resources/js/app.jsx")
            .version("v1")
            .share(|_ctx| Ok(json!({ "appName": "Luxid" }))),
    );

    let response = app
        .get("/")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .send()
        .await
        .assert_ok();

    assert_eq!(response.json()["props"]["appName"], "Luxid");
    assert_eq!(response.json()["props"]["message"], "hello");
}

#[tokio::test]
async fn a_page_prop_wins_over_a_shared_one_of_the_same_name() {
    let app = build(
        Inertia::new("resources/js/app.jsx")
            .version("v1")
            .share(|_ctx| Ok(json!({ "message": "shared" }))),
    );

    let response = app
        .get("/")
        .header("x-inertia", "true")
        .header("x-inertia-version", "v1")
        .send()
        .await;

    assert_eq!(response.json()["props"]["message"], "hello");
}

// ---- escaping -------------------------------------------------------------

#[tokio::test]
async fn the_shell_escapes_props_into_the_attribute() {
    struct XssController;

    #[luxid::controller]
    impl XssController {
        async fn show(ctx: HttpContext) -> Result<Response> {
            ctx.inertia("Home", json!({ "title": "\"><script>alert(1)</script>" }))
        }
    }

    let app = TestApp::new(
        App::new()
            .providers(
                Providers::new().bind::<dyn SessionStore, _>(|_| Arc::new(MemoryStore::new())),
            )
            .routes(|r| {
                r.group("/", |r| {
                    r.middleware(Auth::session());
                    r.middleware(Inertia::new("resources/js/app.jsx"));

                    r.get("/", XssController::show);
                });
            })
            .into_service(),
    );

    let body = app.get("/").send().await.assert_ok().body().to_owned();

    // The payload must not close the attribute or open a tag.
    assert!(!body.contains("<script>alert(1)</script>"), "{body}");
    assert!(body.contains("&lt;script&gt;"), "{body}");
}

// ---- production assets ----------------------------------------------------

#[tokio::test]
async fn a_built_manifest_renders_hashed_asset_tags() {
    let dir = std::env::temp_dir().join(format!("luxid-manifest-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let manifest = dir.join("manifest.json");

    std::fs::write(
        &manifest,
        serde_json::to_string(&json!({
            "resources/js/app.jsx": {
                "file": "assets/app-abc123.js",
                "css": ["assets/app-def456.css"],
                "isEntry": true
            }
        }))
        .unwrap(),
    )
    .expect("write manifest");

    let app = build(
        Inertia::new("resources/js/app.jsx")
            .version("v1")
            .dev(false)
            .manifest(&manifest)
            .asset_base("/build"),
    );

    let body = app.get("/").send().await.assert_ok().body().to_owned();

    assert!(
        body.contains(r#"<script type="module" src="/build/assets/app-abc123.js">"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<link rel="stylesheet" href="/build/assets/app-def456.css">"#),
        "{body}"
    );
    assert!(!body.contains("@vite/client"), "{body}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_missing_manifest_entry_explains_itself() {
    let app = build(
        Inertia::new("resources/js/app.jsx")
            .version("v1")
            .dev(false)
            .manifest("/nonexistent/manifest.json"),
    );

    let body = app.get("/").send().await.assert_ok().body().to_owned();

    // A blank page with no explanation is the worst way to discover that the
    // frontend was never built.
    assert!(body.contains("no manifest entry"), "{body}");
}

// ---- flash, independent of Inertia ----------------------------------------

#[tokio::test]
async fn flash_is_readable_once_then_discarded() {
    pub struct FlashController;

    #[luxid::controller]
    impl FlashController {
        async fn write(ctx: HttpContext) -> Result<Response> {
            ctx.session.flash("notice", "saved")?;
            ctx.response.ok(json!({ "ok": true }))
        }

        async fn read(ctx: HttpContext) -> Result<Response> {
            let notice: Option<String> = ctx.session.flashed("notice")?;
            ctx.response.ok(json!({ "notice": notice }))
        }
    }

    let app = TestApp::new(
        App::new()
            .providers(
                Providers::new().bind::<dyn SessionStore, _>(|_| Arc::new(MemoryStore::new())),
            )
            .routes(|r| {
                r.group("/", |r| {
                    r.middleware(Auth::session());
                    r.post("/write", FlashController::write);
                    r.get("/read", FlashController::read);
                });
            })
            .into_service(),
    );

    let written = app.post("/write").send().await.assert_ok();
    let session = cookie(&written).expect("a session cookie");

    let first = app.get("/read").with_session(&session).send().await;
    assert_eq!(first.json()["notice"], "saved");

    let second = app.get("/read").with_session(&session).send().await;
    assert_eq!(second.json()["notice"], Value::Null);
}
