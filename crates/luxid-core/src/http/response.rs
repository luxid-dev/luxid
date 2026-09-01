//! The owning response half of [`HttpContext`](crate::context::HttpContext).
//!
//! Builder methods return `Response`; terminal methods return
//! `Result<Response>`, so an action ends naturally on `response.ok(users)`.
//! The body stays a structured value rather than pre-serialized bytes so the
//! Inertia adapter can later resolve the same action into either JSON or an
//! HTML shell.

use bytes::Bytes;
use salvo::http::StatusCode;
use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};

/// A cookie to send with a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    name: String,
    value: String,
    path: String,
    max_age: Option<i64>,
    http_only: bool,
    secure: bool,
    same_site: SameSite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    Strict,
    Lax,
    None,
}

impl SameSite {
    fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "Strict",
            Self::Lax => "Lax",
            Self::None => "None",
        }
    }
}

impl Cookie {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            path: "/".to_owned(),
            max_age: None,
            http_only: true,
            secure: false,
            same_site: SameSite::Lax,
        }
    }

    /// A cookie that instructs the client to discard it.
    pub fn removal(name: impl Into<String>) -> Self {
        Self::new(name, "").max_age(0)
    }

    #[must_use]
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    #[must_use]
    pub fn max_age(mut self, seconds: i64) -> Self {
        self.max_age = Some(seconds);
        self
    }

    /// Allow JavaScript to read it. Rarely what you want.
    #[must_use]
    pub fn http_only(mut self, http_only: bool) -> Self {
        self.http_only = http_only;
        self
    }

    /// Send only over HTTPS. Should be on in production.
    #[must_use]
    pub fn secure(mut self, secure: bool) -> Self {
        self.secure = secure;
        self
    }

    #[must_use]
    pub fn same_site(mut self, same_site: SameSite) -> Self {
        self.same_site = same_site;
        self
    }

    pub fn render(&self) -> String {
        let mut out = format!("{}={}; Path={}", self.name, self.value, self.path);

        if let Some(max_age) = self.max_age {
            out.push_str(&format!("; Max-Age={max_age}"));
        }
        if self.http_only {
            out.push_str("; HttpOnly");
        }
        if self.secure {
            out.push_str("; Secure");
        }

        out.push_str(&format!("; SameSite={}", self.same_site.as_str()));
        out
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Empty,
    Json(Value),
    Bytes { data: Bytes, content_type: String },
}

#[derive(Debug)]
pub struct Response {
    pub(crate) status: StatusCode,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Body,
}

impl Default for Response {
    fn default() -> Self {
        Self {
            status: StatusCode::OK,
            headers: Vec::new(),
            body: Body::Empty,
        }
    }
}

impl Response {
    // ---- builder methods -------------------------------------------------

    /// Set the status. An out-of-range code is a programming error and is
    /// surfaced as a 500 rather than a panic.
    #[must_use]
    pub fn status(mut self, code: u16) -> Self {
        self.status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        self
    }

    /// Set a cookie.
    ///
    /// Defaults are the safe ones — `HttpOnly`, `SameSite=Lax`, `Path=/` — so a
    /// session cookie is not reachable from JavaScript unless someone
    /// deliberately makes it so.
    #[must_use]
    pub fn cookie(mut self, cookie: Cookie) -> Self {
        self.headers
            .push(("set-cookie".to_owned(), cookie.render()));
        self
    }

    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    // ---- terminal methods ------------------------------------------------

    /// 200 with a JSON body.
    pub fn ok<T: Serialize>(self, body: T) -> Result<Self> {
        self.status(200).json(body)
    }

    /// 201 with a JSON body.
    pub fn created<T: Serialize>(self, body: T) -> Result<Self> {
        self.status(201).json(body)
    }

    /// 202 with a JSON body.
    pub fn accepted<T: Serialize>(self, body: T) -> Result<Self> {
        self.status(202).json(body)
    }

    /// 204, no body.
    pub fn no_content(mut self) -> Result<Self> {
        self.status = StatusCode::NO_CONTENT;
        self.body = Body::Empty;
        Ok(self)
    }

    /// JSON body, keeping whatever status is already set.
    pub fn json<T: Serialize>(mut self, body: T) -> Result<Self> {
        self.body = Body::Json(serde_json::to_value(body).map_err(|err| {
            Error::Internal(anyhow::anyhow!("could not serialize response: {err}"))
        })?);
        Ok(self)
    }

    pub fn text(mut self, body: impl Into<String>) -> Result<Self> {
        self.body = Body::Bytes {
            data: Bytes::from(body.into()),
            content_type: "text/plain; charset=utf-8".to_owned(),
        };
        Ok(self)
    }

    /// An HTML document. Used by the Inertia shell; also handy for a one-off
    /// page that does not warrant a view layer.
    pub fn html(self, body: impl Into<String>) -> Result<Self> {
        self.bytes(body.into().into_bytes(), "text/html; charset=utf-8")
    }

    pub fn bytes(
        mut self,
        data: impl Into<Bytes>,
        content_type: impl Into<String>,
    ) -> Result<Self> {
        self.body = Body::Bytes {
            data: data.into(),
            content_type: content_type.into(),
        };
        Ok(self)
    }

    /// 303 by default, which is what Inertia requires for PUT/PATCH/DELETE.
    pub fn redirect(mut self, location: impl Into<String>) -> Result<Self> {
        self.status = StatusCode::SEE_OTHER;
        self.headers.push(("location".to_owned(), location.into()));
        self.body = Body::Empty;
        Ok(self)
    }

    // ---- accessors (used by the adapter and by tests) --------------------

    pub fn status_code(&self) -> StatusCode {
        self.status
    }

    pub fn body(&self) -> &Body {
        &self.body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ok_sets_200_and_a_json_body() {
        let res = Response::default()
            .ok(json!({ "id": 1 }))
            .expect("serializes");

        assert_eq!(res.status_code(), StatusCode::OK);
        assert_eq!(res.body(), &Body::Json(json!({ "id": 1 })));
    }

    #[test]
    fn created_sets_201() {
        let res = Response::default()
            .created(json!({ "id": 7 }))
            .expect("serializes");
        assert_eq!(res.status_code(), StatusCode::CREATED);
    }

    #[test]
    fn builders_compose_before_a_terminal_method() {
        let res = Response::default()
            .status(207)
            .header("x-trace", "abc")
            .json(json!([]))
            .expect("serializes");

        assert_eq!(res.status_code().as_u16(), 207);
        assert_eq!(res.headers, vec![("x-trace".to_owned(), "abc".to_owned())]);
    }

    #[test]
    fn no_content_clears_the_body() {
        let res = Response::default()
            .ok(json!({ "a": 1 }))
            .unwrap()
            .no_content()
            .unwrap();

        assert_eq!(res.status_code(), StatusCode::NO_CONTENT);
        assert_eq!(res.body(), &Body::Empty);
    }

    #[test]
    fn an_invalid_status_becomes_500_rather_than_panicking() {
        assert_eq!(
            Response::default().status(9000).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn redirect_uses_303_and_sets_location() {
        let res = Response::default().redirect("/users").expect("redirect");

        assert_eq!(res.status_code(), StatusCode::SEE_OTHER);
        assert_eq!(
            res.headers,
            vec![("location".to_owned(), "/users".to_owned())]
        );
    }
}
