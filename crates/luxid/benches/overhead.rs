//! What does Luxid cost over bare salvo?
//!
//! Three variants serve byte-identical responses:
//!
//! * **bare salvo** — a `#[salvo::handler]` writing JSON directly. The
//!   substrate's ceiling.
//! * **luxid/empty** — a Luxid controller with no middleware and no bound
//!   services. This is the framework's floor: context construction, the
//!   dispatch chain, and response translation, and nothing else.
//! * **luxid/realistic** — global middleware, a JWT guard, and a container with
//!   services resolved per request. What an actual application pays.
//!
//! Requests are driven in-process through `salvo::test::TestClient`, which adds
//! a fixed per-iteration cost to *all three*. That cost does not cancel out of
//! the absolute numbers, so read the **differences** between variants as the
//! framework tax; the absolute figures are a floor on latency, not a
//! throughput claim for a networked server.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use luxid::prelude::*;
use salvo::test::TestClient;
use serde_json::json;
use tokio::runtime::Runtime;

// ---------------------------------------------------------------- bare salvo

#[salvo::handler]
async fn bare_plain(res: &mut salvo::Response) {
    res.render(salvo::writing::Json(json!({ "ok": true })));
}

#[salvo::handler]
async fn bare_lookup(req: &mut salvo::Request, res: &mut salvo::Response) {
    let id = req.param::<i64>("id").unwrap_or_default();
    let page = req.query::<u32>("page").unwrap_or(1);

    res.render(salvo::writing::Json(json!({ "id": id, "page": page })));
}

fn bare_service() -> salvo::Service {
    let router = salvo::Router::new()
        .push(salvo::Router::with_path("plain").get(bare_plain))
        .push(salvo::Router::with_path("users/{id}").get(bare_lookup));

    salvo::Service::new(router)
}

// -------------------------------------------------------------------- luxid

pub struct BenchController;

#[luxid::controller]
impl BenchController {
    async fn plain(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(json!({ "ok": true }))
    }

    async fn lookup(ctx: HttpContext) -> Result<Response> {
        let id: i64 = ctx.params.get("id")?;
        let page = ctx.request.input::<u32>("page")?.unwrap_or(1);

        ctx.response.ok(json!({ "id": id, "page": page }))
    }
}

fn luxid_empty() -> salvo::Service {
    App::new()
        .routes(|r| {
            r.get("/plain", BenchController::plain);
            r.get("/users/{id}", BenchController::lookup);
        })
        .into_service()
}

/// Stands in for the services a real action resolves.
#[derive(Debug)]
struct Settings {
    per_page: u32,
}

#[derive(Debug)]
struct RequestId(u64);

/// A no-op tracing middleware: the shape every app has, without the I/O.
struct Trace;

#[luxid::middleware]
impl Trace {
    async fn handle(&self, ctx: HttpContext, next: Next) -> Result<Response> {
        let response = next.run(ctx).await?;
        Ok(response.header("x-trace", "1"))
    }
}

/// Resolves services the way a real action would.
struct Resolve;

#[luxid::middleware]
impl Resolve {
    async fn handle(&self, mut ctx: HttpContext, next: Next) -> Result<Response> {
        let settings = ctx.services.get::<Settings>()?;
        ctx.extensions.insert(settings.per_page);

        let scoped = ctx.services.get::<RequestId>()?;
        black_box(scoped.0);

        next.run(ctx).await
    }
}

const SECRET: &str = "benchmark-secret-value";

fn luxid_realistic() -> salvo::Service {
    App::new()
        .providers(
            Providers::new()
                .singleton(|_| Settings { per_page: 20 })
                .singleton(|_| Jwt::new(SECRET))
                .scoped(|_| RequestId(1)),
        )
        .middleware(Trace)
        .routes(|r| {
            r.middleware(Resolve);
            r.get("/plain", BenchController::plain)
                .middleware(Auth::jwt());
            r.get("/users/{id}", BenchController::lookup)
                .middleware(Auth::jwt());
        })
        .into_service()
}

/// Middleware and container only — no auth. Isolates what the chain and the
/// container cost from what token verification costs.
fn luxid_middleware_only() -> salvo::Service {
    App::new()
        .providers(
            Providers::new()
                .singleton(|_| Settings { per_page: 20 })
                .scoped(|_| RequestId(1)),
        )
        .middleware(Trace)
        .routes(|r| {
            r.middleware(Resolve);
            r.get("/plain", BenchController::plain);
            r.get("/users/{id}", BenchController::lookup);
        })
        .into_service()
}

/// Auth only — no other middleware, no extra services.
fn luxid_auth_only() -> salvo::Service {
    App::new()
        .providers(Providers::new().singleton(|_| Jwt::new(SECRET)))
        .routes(|r| {
            r.get("/plain", BenchController::plain)
                .middleware(Auth::jwt());
            r.get("/users/{id}", BenchController::lookup)
                .middleware(Auth::jwt());
        })
        .into_service()
}

fn token() -> String {
    Jwt::new(SECRET)
        .sign(&Identity::new("1").with_claim("role", "admin"))
        .expect("signs")
}

// ------------------------------------------------------------------ driving

const BASE: &str = "http://bench.invalid";

/// Every variant sends the same request, including the `authorization` header.
///
/// The variants without a guard simply ignore it. Sending it only to the
/// authenticated variants would charge the driver's own header-building cost to
/// the framework, and that cost is not small next to a 2 µs baseline.
async fn hit(service: &salvo::Service, path: &str, bearer: &str) {
    let request = TestClient::get(format!("{BASE}{path}")).add_header(
        "authorization",
        format!("Bearer {bearer}"),
        true,
    );

    black_box(request.send(service).await);
}

fn benchmarks(c: &mut Criterion) {
    let runtime = Runtime::new().expect("runtime");

    let bare = bare_service();
    let empty = luxid_empty();
    let realistic = luxid_realistic();
    let bearer = token();

    // Sanity: all three must actually answer, or the benchmark measures 404s.
    runtime.block_on(async {
        use salvo::test::ResponseExt;

        for (label, service, auth) in [
            ("bare", &bare, None),
            ("empty", &empty, None),
            ("realistic", &realistic, Some(bearer.as_str())),
        ] {
            let mut request = TestClient::get(format!("{BASE}/plain"));
            if let Some(auth) = auth {
                request = request.add_header("authorization", format!("Bearer {auth}"), true);
            }

            let mut response = request.send(service).await;
            let status = response.status_code.map(|code| code.as_u16()).unwrap_or(0);
            let body = response.take_string().await.unwrap_or_default();

            assert_eq!(status, 200, "{label} did not answer: {body}");
            assert_eq!(body, r#"{"ok":true}"#, "{label} returned a different body");
        }
    });

    let middleware_only = luxid_middleware_only();
    let auth_only = luxid_auth_only();

    // Attribute the cost directly, with no HTTP in the way.
    let verifier = Jwt::new(SECRET);
    c.bench_function("jwt_verify", |b| {
        b.iter(|| black_box(verifier.verify(black_box(&bearer)).expect("verifies")));
    });

    let mut plain = c.benchmark_group("plain");
    plain.measurement_time(Duration::from_secs(8));

    plain.bench_function("salvo", |b| {
        b.to_async(&runtime).iter(|| hit(&bare, "/plain", &bearer));
    });
    plain.bench_function("luxid/empty", |b| {
        b.to_async(&runtime).iter(|| hit(&empty, "/plain", &bearer));
    });
    plain.bench_function("luxid/middleware", |b| {
        b.to_async(&runtime)
            .iter(|| hit(&middleware_only, "/plain", &bearer));
    });
    plain.bench_function("luxid/auth", |b| {
        b.to_async(&runtime)
            .iter(|| hit(&auth_only, "/plain", &bearer));
    });
    plain.bench_function("luxid/realistic", |b| {
        b.to_async(&runtime)
            .iter(|| hit(&realistic, "/plain", &bearer));
    });
    plain.finish();

    let mut lookup = c.benchmark_group("param_and_query");
    lookup.measurement_time(Duration::from_secs(8));

    lookup.bench_function("salvo", |b| {
        b.to_async(&runtime)
            .iter(|| hit(&bare, "/users/7?page=3", &bearer));
    });
    lookup.bench_function("luxid/empty", |b| {
        b.to_async(&runtime)
            .iter(|| hit(&empty, "/users/7?page=3", &bearer));
    });
    lookup.bench_function("luxid/realistic", |b| {
        b.to_async(&runtime)
            .iter(|| hit(&realistic, "/users/7?page=3", &bearer));
    });
    lookup.finish();
}

criterion_group!(overhead, benchmarks);
criterion_main!(overhead);
