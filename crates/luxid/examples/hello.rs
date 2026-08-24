//! A minimal Luxid app: container, middleware, auth, validation-style errors.
//!
//!     cargo run --example hello              # serve (the default)
//!     cargo run --example hello -- routes
//!     cargo run --example hello -- openapi --pretty
//!
//!     curl -i 'localhost:3000/api/v1/users?page=2'
//!     curl localhost:3000/api/v1/users/9
//!     curl -X POST localhost:3000/api/v1/users -d '{"name":""}'
//!
//!     TOKEN=$(curl -s -X POST localhost:3000/api/v1/login \
//!       -d '{"email":"ada@example.com","password":"secret"}' | jq -r .token)
//!     curl -H "authorization: Bearer $TOKEN" localhost:3000/api/v1/me

use std::time::Duration;

// `async_trait` comes from Luxid's migration prelude, so an app need not
// declare it separately.
use luxid::migration::prelude::{MigrationTrait, MigratorTrait, async_trait};
use luxid::prelude::*;
use serde_json::{Value, json};

/// Application settings, resolved from the container by actions.
#[derive(Debug)]
struct Settings {
    app_name: String,
    per_page: u32,
}

/// Times every request. Code before `next.run` runs on the way in, code after
/// runs on the way out — there is no separate after-hook API.
struct Timer;

#[luxid::middleware]
impl Timer {
    async fn handle(&self, ctx: HttpContext, next: Next) -> Result<Response> {
        let started = std::time::Instant::now();
        let response = next.run(ctx).await?;

        Ok(response.header(
            "x-response-time",
            format!("{}ms", started.elapsed().as_millis()),
        ))
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

        // Stands in for a users table until the data layer lands. A wrong
        // email and a wrong password deliberately look identical to a caller.
        if email != "ada@example.com" || password != "secret" {
            return Err(Error::Unauthorized);
        }

        let jwt = ctx.services.get::<Jwt>()?;
        let identity = Identity::new("1").with_claim("role", "admin");

        ctx.response.ok(json!({ "token": jwt.sign(&identity)? }))
    }
}

pub struct MeController;

#[luxid::controller]
impl MeController {
    async fn show(ctx: HttpContext) -> Result<Response> {
        let id: i64 = ctx.auth.id()?;
        let role: Option<String> = ctx.auth.identity()?.claim("role")?;

        ctx.response
            .ok(json!({ "id": id, "role": role, "name": "Ada" }))
    }
}

/// Schemas for the generated document.
#[derive(serde::Serialize, luxid::JsonSchema)]
pub struct UserView {
    pub id: i64,
    pub name: String,
}

#[derive(serde::Deserialize, luxid::JsonSchema)]
pub struct StoreUser {
    pub name: String,
}

pub struct UsersController;

#[luxid::controller]
impl UsersController {
    #[openapi(summary = "List users", tag = "users", ok = UserView)]
    async fn index(ctx: HttpContext) -> Result<Response> {
        let settings = ctx.services.get::<Settings>()?;
        let page = ctx.request.input::<u32>("page")?.unwrap_or(1);

        ctx.response.ok(json!({
            "app": settings.app_name,
            "page": page,
            "per_page": settings.per_page,
            "data": [{ "id": 1, "name": "Ada" }, { "id": 2, "name": "Alan" }],
        }))
    }

    #[openapi(tag = "users", ok = UserView, errors = [404])]
    async fn show(ctx: HttpContext) -> Result<Response> {
        let id: i64 = ctx.params.get("id")?;

        if id != 1 {
            return Err(Error::not_found("User", id));
        }

        ctx.response.ok(json!({ "id": id, "name": "Ada" }))
    }

    #[openapi(tag = "users", body = StoreUser, created = UserView, errors = [422])]
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

        response.created(json!({ "id": 3, "name": name }))
    }
}

/// No migrations yet; the CLI still needs a migrator type.
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        Vec::new()
    }
}

fn build_app() -> App {
    let secret = std::env::var("APP_KEY").unwrap_or_else(|_| "dev-only-insecure-key".to_owned());

    App::new()
        .providers(
            Providers::new()
                .singleton(|_| Settings {
                    app_name: "hello".to_owned(),
                    per_page: 20,
                })
                .singleton(move |_| Jwt::new(&secret).with_ttl(Duration::from_secs(3600))),
        )
        .middleware(Timer)
        .routes(|r| {
            r.group("/api/v1", |r| {
                r.post("/login", AuthController::login);

                r.get("/users", UsersController::index);
                r.post("/users", UsersController::store);
                r.get("/users/{id}", UsersController::show);

                r.get("/me", MeController::show).middleware(Auth::jwt());
            });
        })
}

#[tokio::main]
async fn main() -> luxid::Result<()> {
    luxid::cli::run::<Migrator>(build_app()).await
}
