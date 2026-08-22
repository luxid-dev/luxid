//! `ctx.config` observed through real requests.

use luxid::prelude::*;
use luxid_testing::TestApp;
use serde_json::json;

pub struct SettingsController;

#[luxid::controller]
impl SettingsController {
    async fn show(ctx: HttpContext) -> Result<Response> {
        let name: String = ctx.config.get("app.name")?;
        let per_page: u32 = ctx.config.get_or("app.per_page", 15)?;
        let strict: bool = ctx.config.get_or("database.strict_relations", false)?;

        ctx.response.ok(json!({
            "name": name,
            "per_page": per_page,
            "strict": strict,
        }))
    }

    /// Reads a key nobody set.
    async fn missing(ctx: HttpContext) -> Result<Response> {
        let value: String = ctx.config.get("mail.driver")?;
        ctx.response.ok(json!({ "value": value }))
    }

    /// Reads a key spelled the environment way.
    async fn alias(ctx: HttpContext) -> Result<Response> {
        let per_page: u32 = ctx.config.get("APP_PER_PAGE")?;
        ctx.response.ok(json!({ "per_page": per_page }))
    }
}

fn app() -> TestApp {
    let config = Config::from_pairs([
        ("app.name", "blogapp"),
        ("app.per_page", "20"),
        ("database.strict_relations", "true"),
    ]);

    TestApp::new(
        App::new()
            .config(config)
            .routes(|r| {
                r.get("/settings", SettingsController::show);
                r.get("/missing", SettingsController::missing);
                r.get("/alias", SettingsController::alias);
            })
            .into_service(),
    )
}

#[tokio::test]
async fn an_action_reads_typed_configuration() {
    app()
        .get("/settings")
        .send()
        .await
        .assert_ok()
        .assert_json_path("name", "blogapp")
        .assert_json_path("per_page", 20)
        .assert_json_path("strict", true);
}

#[tokio::test]
async fn the_environment_spelling_reaches_the_same_key() {
    // `APP_PER_PAGE` and `app.per_page` are one key, so a value set either way
    // is readable either way.
    app()
        .get("/alias")
        .send()
        .await
        .assert_ok()
        .assert_json_path("per_page", 20);
}

#[tokio::test]
async fn a_missing_required_key_is_a_redacted_500_that_logs_the_fix() {
    // The client learns nothing; the operator sees which variable to set.
    app()
        .get("/missing")
        .send()
        .await
        .assert_status(500)
        .assert_json_path("title", "internal server error");
}

#[tokio::test]
async fn an_app_with_no_configuration_still_serves() {
    // `App::new()` defaults to the environment, so nothing has to be provided.
    let app = TestApp::new(
        App::new()
            .routes(|r| {
                r.get("/settings", SettingsController::show);
            })
            .into_service(),
    );

    // `app.name` is unset, so this action fails — but the app booted and routed,
    // which is the point.
    app.get("/settings").send().await.assert_status(500);
}

#[test]
fn defaults_apply_only_to_absent_keys() {
    let config = Config::from_pairs([("app.per_page", "20")]);

    assert_eq!(config.get_or("app.per_page", 15).unwrap(), 20);
    assert_eq!(config.get_or("app.missing", 15).unwrap(), 15);
}

#[test]
fn a_toml_file_is_read_and_flattened() {
    let dir = std::env::temp_dir().join("luxid-config-test");
    std::fs::create_dir_all(&dir).expect("temp dir");

    let path = dir.join("luxid.toml");
    std::fs::write(
        &path,
        "[app]\nname = \"from-file\"\n\n[database]\nstrict_relations = true\n",
    )
    .expect("write");

    let config = Config::load(&path).expect("loads");

    assert_eq!(config.get::<String>("app.name").unwrap(), "from-file");
    assert!(config.get::<bool>("database.strict_relations").unwrap());

    std::fs::remove_file(&path).ok();
}
