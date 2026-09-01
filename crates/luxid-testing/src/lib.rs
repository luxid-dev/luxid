//! Test harness for Luxid applications.
//!
//! Requests go through the real service — the same routing, middleware,
//! container and adapter code production runs — so a passing test exercises
//! the actual request path rather than a parallel one.
//!
//! Assertions panic on failure, as test assertions should, and every message
//! includes the response body. A test that fails with only "expected 200, got
//! 500" costs a debugging session that the body would have saved.

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

/// Salvo needs an absolute URL; the host is irrelevant because nothing is bound.
const BASE: &str = "http://test.invalid";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    /// For asserting on a CORS preflight.
    Options,
}

/// An application under test.
#[derive(Clone)]
pub struct TestApp {
    service: Arc<salvo::Service>,
}

impl TestApp {
    pub fn new(service: salvo::Service) -> Self {
        Self {
            service: Arc::new(service),
        }
    }

    pub fn get(&self, path: &str) -> TestRequest {
        self.request(Method::Get, path)
    }

    pub fn post(&self, path: &str) -> TestRequest {
        self.request(Method::Post, path)
    }

    pub fn put(&self, path: &str) -> TestRequest {
        self.request(Method::Put, path)
    }

    pub fn patch(&self, path: &str) -> TestRequest {
        self.request(Method::Patch, path)
    }

    pub fn delete(&self, path: &str) -> TestRequest {
        self.request(Method::Delete, path)
    }

    /// Send an `OPTIONS` request, for asserting on a CORS preflight.
    pub fn options(&self, path: &str) -> TestRequest {
        self.request(Method::Options, path)
    }

    fn request(&self, method: Method, path: &str) -> TestRequest {
        TestRequest {
            app: self.clone(),
            method,
            path: path.to_owned(),
            headers: Vec::new(),
            body: None,
        }
    }
}

pub struct TestRequest {
    app: TestApp,
    method: Method,
    path: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
}

impl TestRequest {
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Send a JSON body.
    #[must_use]
    pub fn json(mut self, body: impl Serialize) -> Self {
        let rendered = serde_json::to_string(&body).expect("the test body serializes");

        self.body = Some(rendered);
        self.header("content-type", "application/json")
    }

    /// Authenticate as the bearer of `token`.
    #[must_use]
    pub fn bearer(self, token: impl std::fmt::Display) -> Self {
        self.header("authorization", format!("Bearer {token}"))
    }

    /// Act as a subject, skipping the login round-trip.
    ///
    /// Signs a token with `secret` — the same one the application's `Jwt` is
    /// configured with — so the request goes through the real guard rather
    /// than around it. A test that bypassed the guard would not be testing the
    /// guard.
    #[must_use]
    pub fn acting_as(self, secret: &str, subject: impl std::fmt::Display) -> Self {
        let token = luxid_core::Jwt::new(secret)
            .sign(&luxid_core::Identity::new(subject.to_string()))
            .expect("signing a test token");

        self.bearer(token)
    }

    /// Act as a subject carrying extra claims.
    #[must_use]
    pub fn acting_as_with(
        self,
        secret: &str,
        subject: impl std::fmt::Display,
        claims: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> Self {
        let mut identity = luxid_core::Identity::new(subject.to_string());

        for (name, value) in claims {
            identity = identity.with_claim(name, value);
        }

        let token = luxid_core::Jwt::new(secret)
            .sign(&identity)
            .expect("signing a test token");

        self.bearer(token)
    }

    /// Present a session cookie.
    #[must_use]
    pub fn with_session(self, cookie: impl std::fmt::Display) -> Self {
        self.header("cookie", format!("luxid_session={cookie}"))
    }

    pub async fn send(self) -> TestResponse {
        let url = format!("{BASE}{}", self.path);

        let mut builder = match self.method {
            Method::Get => salvo::test::TestClient::get(url),
            Method::Post => salvo::test::TestClient::post(url),
            Method::Put => salvo::test::TestClient::put(url),
            Method::Patch => salvo::test::TestClient::patch(url),
            Method::Delete => salvo::test::TestClient::delete(url),
            Method::Options => salvo::test::TestClient::options(url),
        };

        for (name, value) in self.headers {
            // Header names are test-supplied strings; a malformed one should
            // fail the test loudly rather than be dropped.
            let name = salvo::http::HeaderName::try_from(name.as_str())
                .unwrap_or_else(|_| panic!("`{name}` is not a valid header name"));

            builder = builder.add_header(name, value, true);
        }
        if let Some(body) = self.body {
            builder = builder.text(body);
        }

        let mut response = builder.send(self.app.service.as_ref()).await;

        let status = response
            .status_code
            .map(|code| code.as_u16())
            .unwrap_or(200);
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
            })
            .collect();

        use salvo::test::ResponseExt;
        let body = response.take_string().await.unwrap_or_default();

        TestResponse {
            status,
            headers,
            body,
        }
    }
}

pub struct TestResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl TestResponse {
    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The body as JSON, panicking with the raw body if it is not JSON — which
    /// is usually a 500 page and exactly what you need to see.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|err| {
            panic!(
                "expected a JSON body but could not parse it: {err}\nbody: {}",
                self.body
            )
        })
    }

    pub fn assert_status(self, expected: u16) -> Self {
        assert!(
            self.status == expected,
            "expected status {expected}, got {}\nbody: {}",
            self.status,
            self.body
        );
        self
    }

    pub fn assert_ok(self) -> Self {
        self.assert_status(200)
    }

    pub fn assert_created(self) -> Self {
        self.assert_status(201)
    }

    pub fn assert_no_content(self) -> Self {
        self.assert_status(204)
    }

    pub fn assert_unauthorized(self) -> Self {
        self.assert_status(401)
    }

    pub fn assert_forbidden(self) -> Self {
        self.assert_status(403)
    }

    pub fn assert_not_found(self) -> Self {
        self.assert_status(404)
    }

    pub fn assert_header(self, name: &str, expected: &str) -> Self {
        match self.header(name) {
            Some(actual) => assert!(
                actual == expected,
                "expected header `{name}: {expected}`, got `{actual}`"
            ),
            None => panic!(
                "expected header `{name}` but it was absent. Present: {:?}",
                self.headers.iter().map(|(key, _)| key).collect::<Vec<_>>()
            ),
        }
        self
    }

    /// Assert a value at a dotted path: `data.0.name`.
    pub fn assert_json_path(self, path: &str, expected: impl Into<Value>) -> Self {
        let expected = expected.into();
        let json = self.json();

        match resolve(&json, path) {
            Some(actual) => assert!(
                *actual == expected,
                "at `{path}`: expected {expected}, got {actual}\nbody: {}",
                self.body
            ),
            None => panic!("no value at `{path}`\nbody: {}", self.body),
        }
        self
    }

    /// Assert the length of an array at a dotted path.
    pub fn assert_json_count(self, path: &str, expected: usize) -> Self {
        let json = self.json();

        let Some(value) = resolve(&json, path) else {
            panic!("no value at `{path}`\nbody: {}", self.body);
        };
        let Some(array) = value.as_array() else {
            panic!("`{path}` is not an array\nbody: {}", self.body);
        };

        assert!(
            array.len() == expected,
            "at `{path}`: expected {expected} items, got {}\nbody: {}",
            array.len(),
            self.body
        );
        self
    }

    /// Assert a specific validation message for a field.
    ///
    /// Saves reaching for `assert_json_path("errors.email.0", ..)` and getting
    /// the prefix wrong, which is the mistake everyone makes once.
    pub fn assert_validation_message(self, field: &str, expected: &str) -> Self {
        let json = self.json();

        let Some(messages) = json
            .pointer(&format!("/errors/{field}"))
            .and_then(Value::as_array)
        else {
            panic!("no validation errors for `{field}`\nbody: {}", self.body);
        };

        let found = messages
            .iter()
            .filter_map(Value::as_str)
            .any(|message| message == expected);

        assert!(
            found,
            "expected `{field}` to report {expected:?}, got {:?}\nbody: {}",
            messages
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>(),
            self.body
        );
        self
    }

    /// Assert a 422 naming exactly these fields — the shape Luxid's validation
    /// errors take.
    pub fn assert_validation_errors(self, fields: &[&str]) -> Self {
        let response = self.assert_status(422);
        let json = response.json();

        let Some(errors) = json.get("errors").and_then(Value::as_object) else {
            panic!("expected an `errors` object\nbody: {}", response.body);
        };

        for field in fields {
            assert!(
                errors.contains_key(*field),
                "expected a validation error for `{field}`, got {:?}\nbody: {}",
                errors.keys().collect::<Vec<_>>(),
                response.body
            );
        }

        assert!(
            errors.len() == fields.len(),
            "expected errors for exactly {:?}, got {:?}",
            fields,
            errors.keys().collect::<Vec<_>>()
        );
        response
    }
}

/// Resolve a dotted path, treating numeric segments as array indices.
fn resolve<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;

    for segment in path.split('.').filter(|segment| !segment.is_empty()) {
        current = match segment.parse::<usize>() {
            Ok(index) => current.get(index)?,
            Err(_) => current.get(segment)?,
        };
    }

    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(status: u16, body: Value) -> TestResponse {
        TestResponse {
            status,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }

    #[test]
    fn resolves_object_and_array_segments() {
        let value = json!({ "data": [{ "name": "Ada" }, { "name": "Alan" }] });

        assert_eq!(resolve(&value, "data.1.name"), Some(&json!("Alan")));
        assert_eq!(resolve(&value, "data"), value.get("data"));
        assert_eq!(resolve(&value, "data.9.name"), None);
        assert_eq!(resolve(&value, "missing"), None);
    }

    #[test]
    fn passing_assertions_return_the_response_for_chaining() {
        response(200, json!({ "data": [1, 2, 3], "page": 1 }))
            .assert_ok()
            .assert_json_count("data", 3)
            .assert_json_path("page", 1);
    }

    #[test]
    #[should_panic(expected = "expected status 200, got 500")]
    fn a_wrong_status_fails_with_the_body() {
        response(500, json!({ "title": "boom" })).assert_ok();
    }

    #[test]
    #[should_panic(expected = "at `page`: expected 2, got 1")]
    fn a_wrong_json_value_reports_both_sides() {
        response(200, json!({ "page": 1 })).assert_json_path("page", 2);
    }

    #[test]
    #[should_panic(expected = "no value at `nope`")]
    fn a_missing_path_names_the_path() {
        response(200, json!({ "page": 1 })).assert_json_path("nope", 1);
    }

    #[test]
    fn validation_errors_match_exactly_the_named_fields() {
        response(
            422,
            json!({ "errors": { "email": ["is invalid"], "name": ["is required"] } }),
        )
        .assert_validation_errors(&["email", "name"]);
    }

    #[test]
    #[should_panic(expected = "expected a validation error for `name`")]
    fn a_missing_validation_field_is_reported() {
        response(422, json!({ "errors": { "email": ["is invalid"] } }))
            .assert_validation_errors(&["name"]);
    }

    #[test]
    #[should_panic(expected = "expected errors for exactly")]
    fn extra_validation_fields_are_reported_too() {
        response(
            422,
            json!({ "errors": { "email": ["is invalid"], "name": ["is required"] } }),
        )
        .assert_validation_errors(&["email"]);
    }
}
