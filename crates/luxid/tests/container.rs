//! Container behaviour observed through real requests.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use luxid::__private::salvo::test::{ResponseExt, TestClient};
use luxid::prelude::*;
use serde_json::{Value, json};

/// Each instance takes the next id, so identity is visible in the response.
#[derive(Debug)]
struct Tracked {
    id: usize,
}

impl Tracked {
    fn next(counter: &Arc<AtomicUsize>) -> Self {
        Self {
            id: counter.fetch_add(1, Ordering::SeqCst),
        }
    }
}

trait Greeter: Send + Sync {
    fn greet(&self) -> String;
}

struct English;
impl Greeter for English {
    fn greet(&self) -> String {
        "hello".to_owned()
    }
}

struct Pirate;
impl Greeter for Pirate {
    fn greet(&self) -> String {
        "ahoy".to_owned()
    }
}

/// Resolves the scoped service on the way in and stashes its id, so the action
/// can prove it received the same instance.
struct RecordScoped;

#[luxid::middleware]
impl RecordScoped {
    async fn handle(&self, mut ctx: HttpContext, next: Next) -> Result<Response> {
        let seen = ctx.services.get::<Tracked>()?.id;
        ctx.extensions.insert(SeenInMiddleware(seen));

        next.run(ctx).await
    }
}

#[derive(Debug)]
struct SeenInMiddleware(usize);

pub struct ProbeController;

#[luxid::controller]
impl ProbeController {
    async fn identity(ctx: HttpContext) -> Result<Response> {
        let tracked = ctx.services.get::<Tracked>()?;
        let in_middleware = ctx.extensions.get::<SeenInMiddleware>().map(|seen| seen.0);

        ctx.response
            .ok(json!({ "id": tracked.id, "middleware_saw": in_middleware }))
    }

    async fn greet(ctx: HttpContext) -> Result<Response> {
        let greeter = ctx.services.get_dyn::<dyn Greeter>()?;
        ctx.response.ok(json!({ "greeting": greeter.greet() }))
    }

    async fn missing(ctx: HttpContext) -> Result<Response> {
        let absent = ctx.services.get::<Unregistered>()?;
        ctx.response.ok(json!({ "unreachable": absent.0 }))
    }
}

#[derive(Debug)]
struct Unregistered(u8);

fn app(providers: Providers) -> luxid::__private::salvo::Service {
    App::new()
        .providers(providers)
        .routes(|r| {
            r.get("/identity", ProbeController::identity)
                .middleware(RecordScoped);
            r.get("/greet", ProbeController::greet);
            r.get("/missing", ProbeController::missing);
        })
        .into_service()
}

const BASE: &str = "http://127.0.0.1:5800";

async fn identity(service: &luxid::__private::salvo::Service) -> Value {
    let mut res = TestClient::get(format!("{BASE}/identity"))
        .send(service)
        .await;
    res.take_json().await.expect("json body")
}

#[tokio::test]
async fn a_singleton_is_shared_across_requests() {
    let counter = Arc::new(AtomicUsize::new(0));
    let service = app(Providers::new().singleton(move |_| Tracked::next(&counter)));

    let first = identity(&service).await;
    let second = identity(&service).await;

    assert_eq!(first["id"], 0);
    assert_eq!(second["id"], 0, "the same instance serves every request");
}

#[tokio::test]
async fn a_scoped_service_is_fresh_per_request_but_shared_within_one() {
    let counter = Arc::new(AtomicUsize::new(0));
    let service = app(Providers::new().scoped(move |_| Tracked::next(&counter)));

    let first = identity(&service).await;
    let second = identity(&service).await;

    // Middleware and action resolved the same instance inside one request...
    assert_eq!(first["middleware_saw"], first["id"]);
    assert_eq!(second["middleware_saw"], second["id"]);

    // ...and the next request got a different one.
    assert_ne!(first["id"], second["id"]);
}

#[tokio::test]
async fn a_transient_service_differs_between_middleware_and_action() {
    let counter = Arc::new(AtomicUsize::new(0));
    let service = app(Providers::new().transient(move |_| Tracked::next(&counter)));

    let body = identity(&service).await;
    assert_ne!(body["middleware_saw"], body["id"]);
}

#[tokio::test]
async fn a_bound_implementation_can_be_swapped_for_a_test() {
    let production = app(Providers::new().bind::<dyn Greeter, _>(|_| Arc::new(English)));
    let under_test = app(Providers::new().bind::<dyn Greeter, _>(|_| Arc::new(Pirate)));

    let mut res = TestClient::get(format!("{BASE}/greet"))
        .send(&production)
        .await;
    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["greeting"], "hello");

    let mut res = TestClient::get(format!("{BASE}/greet"))
        .send(&under_test)
        .await;
    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["greeting"], "ahoy");
}

#[tokio::test]
async fn an_unresolvable_service_is_a_redacted_500() {
    let service = app(Providers::new());

    let mut res = TestClient::get(format!("{BASE}/missing"))
        .send(&service)
        .await;
    assert_eq!(res.status_code.map(|s| s.as_u16()), Some(500));

    let body: Value = res.take_json().await.expect("json body");
    assert_eq!(body["title"], "internal server error");
    assert!(
        !body.to_string().contains("Unregistered"),
        "internal details must not reach the client"
    );
}

#[test]
fn a_missing_binding_fails_at_boot_naming_the_type() {
    let err = App::new()
        .providers(Providers::new().try_singleton(|c| {
            c.get::<Unregistered>()
                .map(|v| Tracked { id: v.0 as usize })
        }))
        .routes(|r| {
            r.get("/identity", ProbeController::identity);
        })
        .try_into_service()
        .expect_err("the graph is incomplete");

    let message = format!("{err}");
    assert!(message.contains("Unregistered"), "{message}");
    assert!(message.contains("providers()"), "{message}");
}

#[test]
fn a_complete_graph_boots() {
    let counter = Arc::new(AtomicUsize::new(0));

    App::new()
        .providers(Providers::new().singleton(move |_| Tracked::next(&counter)))
        .routes(|r| {
            r.get("/identity", ProbeController::identity);
        })
        .try_into_service()
        .expect("the graph is complete");
}
