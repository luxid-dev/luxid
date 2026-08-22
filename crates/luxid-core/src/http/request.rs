//! The owning request half of [`HttpContext`](crate::context::HttpContext).
//!
//! Luxid's `Request` owns its parts rather than borrowing salvo's. That is
//! what allows actions to be written `async fn index(ctx: HttpContext)` with
//! no lifetime parameter — edition 2024 makes elided lifetimes in paths a hard
//! error (E0726), so a borrowing context would force `HttpContext<'_>`
//! everywhere. Owning also keeps salvo entirely out of user-facing signatures.

use std::collections::HashMap;
use std::sync::OnceLock;

use bytes::Bytes;
use salvo::http::uri::Uri;
use salvo::http::{HeaderMap, Method};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Error, Result};

#[derive(Debug)]
pub struct Request {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    queries: HashMap<String, Vec<String>>,
    body: Bytes,
    /// Parsed lazily: most requests never look at the JSON body, and a GET
    /// should not pay for a parse attempt.
    ///
    /// `OnceLock` rather than `OnceCell` because `validate` borrows the request
    /// across an await, and a future holding `&Request` is only `Send` if
    /// `Request` is `Sync`.
    json: OnceLock<Option<Value>>,
}

impl Request {
    pub(crate) fn new(
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        queries: HashMap<String, Vec<String>>,
        body: Bytes,
    ) -> Self {
        Self {
            method,
            uri,
            headers,
            queries,
            body,
            json: OnceLock::new(),
        }
    }

    pub fn method(&self) -> &Method {
        &self.method
    }

    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    pub fn path(&self) -> &str {
        self.uri.path()
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// A cookie sent by the client.
    ///
    /// Parsed on demand from the `Cookie` header: most requests never ask, and
    /// the ones that do usually ask once.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        let header = self.header("cookie")?;

        header.split(';').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;

            (key.trim() == name).then(|| value.trim())
        })
    }

    pub fn bearer_token(&self) -> Option<&str> {
        self.header("authorization")?
            .strip_prefix("Bearer ")
            .map(str::trim)
    }

    pub fn body_bytes(&self) -> &Bytes {
        &self.body
    }

    /// First value of a query-string key, decoded into `T`.
    pub fn query<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.queries.get(key).and_then(|values| values.first()) {
            Some(raw) => decode_scalar(key, raw).map(Some),
            None => Ok(None),
        }
    }

    /// Every value of a repeated query-string key (`?tag=a&tag=b`).
    pub fn query_all<T: DeserializeOwned>(&self, key: &str) -> Result<Vec<T>> {
        self.queries
            .get(key)
            .map(|values| values.iter().map(|raw| decode_scalar(key, raw)).collect())
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    /// Query string first, then the JSON body — Adonis's `request.input()`.
    pub fn input<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        if let Some(found) = self.query(key)? {
            return Ok(Some(found));
        }

        match self.json_body().and_then(|json| json.get(key)) {
            Some(value) => serde_json::from_value(value.clone())
                .map(Some)
                .map_err(|err| Error::BadRequest(format!("`{key}` in body: {err}"))),
            None => Ok(None),
        }
    }

    /// Deserialize the whole JSON body.
    pub fn body_json<T: DeserializeOwned>(&self) -> Result<T> {
        if self.body.is_empty() {
            return Err(Error::BadRequest("a JSON body is required".into()));
        }
        Ok(serde_json::from_slice(&self.body)?)
    }

    /// Deserialize the JSON body and run its rules, or fail with a 422
    /// listing every problem at once.
    pub async fn validate<T>(&self) -> Result<T>
    where
        T: DeserializeOwned + crate::validation::Validate,
    {
        let value: T = self.body_json()?;
        crate::validation::validate(&value).await?;

        Ok(value)
    }

    fn json_body(&self) -> Option<&Value> {
        self.json
            .get_or_init(|| serde_json::from_slice(&self.body).ok())
            .as_ref()
    }
}

/// Query strings are untyped text, so `?page=2` must reach `u32` and
/// `?name=ada` must reach `String`. Try the raw text as JSON first (which
/// handles numbers, booleans and null), then fall back to treating it as a
/// JSON string.
pub(crate) fn decode_scalar<T: DeserializeOwned>(key: &str, raw: &str) -> Result<T> {
    if let Ok(value) = serde_json::from_str::<T>(raw) {
        return Ok(value);
    }

    serde_json::from_value(Value::String(raw.to_owned()))
        .map_err(|err| Error::BadRequest(format!("`{key}` could not be read from `{raw}`: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(query: &[(&str, &str)], body: &str) -> Request {
        let mut queries: HashMap<String, Vec<String>> = HashMap::new();
        for (key, value) in query {
            queries
                .entry((*key).to_owned())
                .or_default()
                .push((*value).to_owned());
        }

        Request::new(
            Method::POST,
            "/users?x=1".parse().expect("valid uri"),
            HeaderMap::new(),
            queries,
            Bytes::copy_from_slice(body.as_bytes()),
        )
    }

    #[test]
    fn decodes_typed_query_values() {
        let req = request(&[("page", "2"), ("name", "ada"), ("active", "true")], "");

        assert_eq!(req.query::<u32>("page").unwrap(), Some(2));
        assert_eq!(req.query::<String>("name").unwrap(), Some("ada".into()));
        assert_eq!(req.query::<bool>("active").unwrap(), Some(true));
        assert_eq!(req.query::<u32>("missing").unwrap(), None);
    }

    #[test]
    fn reports_undecodable_query_values() {
        let req = request(&[("page", "not-a-number")], "");
        let err = req.query::<u32>("page").unwrap_err();

        assert_eq!(err.status_code(), salvo::http::StatusCode::BAD_REQUEST);
        assert!(err.to_string().contains("page"));
    }

    #[test]
    fn collects_repeated_query_values() {
        let req = request(&[("tag", "a"), ("tag", "b")], "");
        assert_eq!(req.query_all::<String>("tag").unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn input_prefers_query_then_falls_back_to_body() {
        let req = request(&[("page", "9")], r#"{"page": 1, "search": "ada"}"#);

        assert_eq!(req.input::<u32>("page").unwrap(), Some(9));
        assert_eq!(req.input::<String>("search").unwrap(), Some("ada".into()));
        assert_eq!(req.input::<String>("absent").unwrap(), None);
    }

    #[test]
    fn body_json_rejects_an_empty_body() {
        let err = request(&[], "").body_json::<Value>().unwrap_err();
        assert_eq!(err.status_code(), salvo::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn extracts_a_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer abc.def".parse().expect("header"));
        let req = Request::new(
            Method::GET,
            "/".parse().expect("valid uri"),
            headers,
            HashMap::new(),
            Bytes::new(),
        );

        assert_eq!(req.bearer_token(), Some("abc.def"));
    }
}
