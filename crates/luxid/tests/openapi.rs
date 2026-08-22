//! `#[openapi(..)]` end to end: attributes on real actions, schemas from real
//! types, and the document assembled from the route table.

use luxid::JsonSchema;
use luxid::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema, Validate)]
pub struct StorePost {
    #[validate(length(min = 1, max = 200))]
    pub title: String,
    pub draft: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PostView {
    pub id: i64,
    pub title: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PageOfPosts {
    pub data: Vec<PostView>,
    pub total: u64,
}

pub struct PostsController;

#[luxid::controller]
impl PostsController {
    #[openapi(summary = "List posts", tag = "posts", ok = PageOfPosts)]
    async fn index(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(PageOfPosts {
            data: Vec::new(),
            total: 0,
        })
    }

    #[openapi(tag = "posts", ok = PostView, errors = [404])]
    async fn show(ctx: HttpContext) -> Result<Response> {
        let id: i64 = ctx.params.get("id")?;
        ctx.response.ok(PostView {
            id,
            title: "First".into(),
        })
    }

    #[openapi(body = StorePost, created = PostView, errors = [422, 409])]
    async fn store(ctx: HttpContext) -> Result<Response> {
        let input: StorePost = ctx.request.validate().await?;
        ctx.response.created(PostView {
            id: 1,
            title: input.title,
        })
    }

    #[openapi(no_content, errors = [404])]
    async fn destroy(ctx: HttpContext) -> Result<Response> {
        ctx.response.no_content()
    }

    /// Deliberately undocumented.
    async fn health(ctx: HttpContext) -> Result<Response> {
        ctx.response.ok(serde_json::json!({ "ok": true }))
    }
}

fn app() -> App {
    App::new().routes(|r| {
        r.group("/api", |r| {
            r.get("/posts", PostsController::index);
            r.post("/posts", PostsController::store);
            r.get("/posts/{id}", PostsController::show);
            r.delete("/posts/{id}", PostsController::destroy);
            r.get("/health", PostsController::health);
        });
    })
}

fn document() -> Value {
    app().openapi("Posts API", "1.2.3")
}

#[test]
fn the_document_is_openapi_3_1_with_the_given_info() {
    let document = document();

    assert_eq!(document["openapi"], "3.1.0");
    assert_eq!(document["info"]["title"], "Posts API");
    assert_eq!(document["info"]["version"], "1.2.3");
}

#[test]
fn every_registered_route_appears() {
    let paths = document();
    let paths = paths["paths"].as_object().expect("object");

    assert_eq!(paths.len(), 3, "/posts, /posts/{{id}}, /health");
    assert!(paths["/api/posts"].get("get").is_some());
    assert!(paths["/api/posts"].get("post").is_some());
    assert!(paths["/api/posts/{id}"].get("get").is_some());
    assert!(paths["/api/posts/{id}"].get("delete").is_some());
}

#[test]
fn summaries_tags_and_operation_ids_come_from_the_attribute() {
    let document = document();
    let index = &document["paths"]["/api/posts"]["get"];

    assert_eq!(index["summary"], "List posts");
    assert_eq!(index["tags"][0], "posts");
    assert_eq!(index["operationId"], "PostsController::index");
}

#[test]
fn response_schemas_are_generated_from_the_real_types() {
    let document = document();
    let schema = &document["paths"]["/api/posts"]["get"]["responses"]["200"]["content"]["application/json"]
        ["schema"];

    // Derived from `PageOfPosts`, not hand-written.
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"].get("total").is_some());
    assert!(schema["properties"].get("data").is_some());
    assert_eq!(schema["properties"]["data"]["type"], "array");
}

#[test]
fn a_request_body_schema_is_generated_and_required() {
    let document = document();
    let store = &document["paths"]["/api/posts"]["post"];

    assert_eq!(store["requestBody"]["required"], true);

    let schema = &store["requestBody"]["content"]["application/json"]["schema"];
    assert_eq!(schema["properties"]["title"]["type"], "string");
    assert_eq!(schema["properties"]["draft"]["type"], "boolean");
}

#[test]
fn error_statuses_reference_the_problem_document() {
    let document = document();
    let store = &document["paths"]["/api/posts"]["post"]["responses"];

    assert_eq!(store["201"]["description"], "Created");
    assert_eq!(store["422"]["description"], "The given data was invalid");
    assert_eq!(store["409"]["description"], "Conflict");

    // Nobody declared this shape; it comes from Luxid's own error rendering.
    assert_eq!(
        store["422"]["content"]["application/problem+json"]["schema"]["$ref"],
        "#/components/schemas/Problem"
    );

    let problem = &document["components"]["schemas"]["Problem"];
    assert_eq!(problem["properties"]["status"]["type"], "integer");
    assert!(problem["properties"].get("errors").is_some());
}

#[test]
fn path_parameters_are_derived_from_the_route_pattern() {
    let document = document();
    let show = &document["paths"]["/api/posts/{id}"]["get"];

    assert_eq!(show["parameters"][0]["name"], "id");
    assert_eq!(show["parameters"][0]["in"], "path");
    assert_eq!(show["parameters"][0]["required"], true);
}

#[test]
fn no_content_records_a_204_with_no_body() {
    let document = document();
    let destroy = &document["paths"]["/api/posts/{id}"]["delete"]["responses"];

    assert_eq!(destroy["204"]["description"], "No content");
    assert!(destroy["204"].get("content").is_none());
}

#[test]
fn an_undocumented_action_is_present_but_bare() {
    let document = document();
    let health = &document["paths"]["/api/health"]["get"];

    // Present, so the spec never silently omits an endpoint...
    assert_eq!(health["operationId"], "PostsController::health");
    // ...but nothing is invented for it.
    assert_eq!(
        health["responses"]["default"]["description"],
        "Undocumented"
    );
}

#[test]
fn the_document_survives_a_json_round_trip() {
    let rendered = serde_json::to_string(&document()).expect("serializes");
    let parsed: Value = serde_json::from_str(&rendered).expect("parses");

    assert_eq!(parsed["openapi"], "3.1.0");
}
