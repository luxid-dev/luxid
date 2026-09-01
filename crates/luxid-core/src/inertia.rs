//! The server half of the [Inertia.js](https://inertiajs.com) protocol.
//!
//! Inertia lets a React/Vue/Svelte frontend be driven by ordinary server-side
//! routing and controllers: no API client, no client-side router, no token
//! juggling. The official client adapters work against any backend that speaks
//! the protocol, so Luxid only has to implement this half.
//!
//! # The protocol, in full
//!
//! | Condition | Response |
//! |---|---|
//! | no `X-Inertia` header | an HTML shell containing `<div id="app" data-page="…">` |
//! | `X-Inertia: true` | JSON `{component, props, url, version}` + `X-Inertia: true` |
//! | `X-Inertia-Version` stale on a GET | `409` + `X-Inertia-Location`, forcing a hard reload |
//! | `X-Inertia-Partial-Data` present | as above, with `props` filtered to the named keys |
//! | validation failure | `303` back to the previous page, errors flashed to the session |
//!
//! That last row is the one that shapes everything else. Inertia is built on
//! post-redirect-get: a failed form does not render an error document, it
//! bounces back to the page it came from and the errors arrive as a prop.
//!
//! # Why this is middleware rather than error handling
//!
//! The obvious place to turn a validation failure into a redirect is Luxid's
//! error renderer. It cannot work there: by the time `write_error` runs, the
//! `HttpContext` — and with it the `Session` the errors must be flashed to —
//! has already been consumed by the middleware chain.
//!
//! [`Inertia`] therefore does what [`crate::auth::SessionGuard`] does: it keeps
//! a handle to the session on the way in, and converts the failure into a
//! response on the way out.
//!
//! # Ordering matters
//!
//! `Auth::session()` must sit **outside** this middleware:
//!
//! ```ignore
//! r.middleware((Auth::session(), Inertia::new("resources/js/app.jsx")));
//! ```
//!
//! The session guard persists on the way out with `next.run(ctx).await?`, so an
//! `Err` never reaches its write-back. This middleware converts the `Err` into
//! an `Ok(redirect)` first, which is what lets the flashed errors survive.
//! Reversed, the flash would be written and then silently dropped.
//!
//! # What this does not change
//!
//! Nothing about [`crate::error::Error`]. A route without this middleware still
//! answers a validation failure with `422 application/problem+json`, so a JSON
//! API and an Inertia frontend can share the same actions, the same validators
//! and the same `ctx.request.validate::<T>()` call. Which rendering you get is
//! decided by which route group you are in.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::context::HttpContext;
use crate::error::{Error, Result};
use crate::http::Response;
use crate::middleware::{BoxFuture, Middleware, Next};

/// Session key holding the page a form was submitted from.
const PREVIOUS_URL: &str = "__luxid_previous_url";

/// Flash key holding validation errors for the next request.
const ERRORS: &str = "errors";

// ---------------------------------------------------------------------------
// The per-request handle that lives on HttpContext
// ---------------------------------------------------------------------------

/// The request's Inertia state, set by the [`Inertia`] middleware.
///
/// Detached when the middleware did not run, in which case [`HttpContext::inertia`]
/// fails with a message saying so rather than rendering something subtly wrong.
#[derive(Debug, Clone, Default)]
pub struct InertiaRequest {
    inner: Option<Arc<RequestState>>,
}

#[derive(Debug)]
struct RequestState {
    /// True when the client sent `X-Inertia`, i.e. this is an XHR-style
    /// navigation rather than a fresh browser load.
    is_inertia: bool,
    url: String,
    version: Option<String>,
    shared: Value,
    shell: Arc<Shell>,
    /// `X-Inertia-Partial-Data`, when it applies to the component being rendered.
    only: Option<Vec<String>>,
    except: Option<Vec<String>>,
    partial_component: Option<String>,
}

impl InertiaRequest {
    /// Whether the current request came from the Inertia client.
    ///
    /// Useful for the rare action that needs to behave differently on a fresh
    /// page load, e.g. skipping an expensive prop the client already has.
    pub fn is_inertia_request(&self) -> bool {
        self.inner.as_ref().is_some_and(|state| state.is_inertia)
    }

    /// Whether the middleware ran on this route.
    pub fn is_active(&self) -> bool {
        self.inner.is_some()
    }

    fn state(&self) -> Result<&RequestState> {
        self.inner.as_deref().ok_or_else(|| {
            Error::internal(
                "this route does not have Inertia enabled. Add \
                 `.middleware((Auth::session(), Inertia::new(\"resources/js/app.jsx\")))` \
                 to the route group.",
            )
        })
    }
}

// ---------------------------------------------------------------------------
// Rendering a page
// ---------------------------------------------------------------------------

impl HttpContext {
    /// Render an Inertia page.
    ///
    /// ```ignore
    /// async fn index(ctx: HttpContext) -> Result<Response> {
    ///     let todos = Todo::owned_by(ctx.auth.id()?).paginate(1, 50).await?;
    ///
    ///     ctx.inertia("Todos/Index", json!({ "todos": todos }))
    /// }
    /// ```
    ///
    /// `component` is a path into the client's page directory —
    /// `"Todos/Index"` resolves to `resources/js/Pages/Todos/Index.jsx`.
    ///
    /// The same call serves both a fresh browser load (an HTML shell) and an
    /// Inertia navigation (JSON). The action does not know or care which.
    ///
    /// Consumes the context so any headers or cookies already set on
    /// `ctx.response` are preserved.
    pub fn inertia(self, component: &str, props: impl Serialize) -> Result<Response> {
        let props = serde_json::to_value(props)
            .map_err(|err| Error::internal(format!("inertia props for `{component}`: {err}")))?;

        let state = self.inertia.state()?;
        let response = self.response;

        // Shared props first, so a page prop of the same name wins. That order
        // is what lets one page override a globally shared value.
        let mut merged = match state.shared.clone() {
            Value::Object(map) => map,
            _ => Map::new(),
        };

        if let Value::Object(map) = props {
            merged.extend(map);
        }

        // Partial reloads: the client asks for a subset of the props it already
        // knows the names of. Only honoured when the component matches — a
        // partial request that lands on a different component is a full render,
        // or the client would receive a page missing most of its data.
        let component_matches = state
            .partial_component
            .as_deref()
            .is_some_and(|name| name == component);

        if component_matches {
            if let Some(only) = &state.only {
                merged.retain(|key, _| only.iter().any(|wanted| wanted == key));
            }
            if let Some(except) = &state.except {
                merged.retain(|key, _| !except.iter().any(|unwanted| unwanted == key));
            }
        }

        let mut page = json!({
            "component": component,
            "props": Value::Object(merged),
            "url": state.url,
        });

        // The client compares this against its own copy and triggers a hard
        // reload when they differ, which is how a deploy reaches open tabs.
        if let Some(version) = &state.version {
            page["version"] = json!(version);
        }

        if state.is_inertia {
            return response
                .header("x-inertia", "true")
                // Without this, a cache can serve the JSON payload to a plain
                // browser navigation, and the user gets a screenful of JSON.
                .header("vary", "X-Inertia")
                .json(page);
        }

        response.html(state.shell.render(&page))
    }
}

// ---------------------------------------------------------------------------
// The HTML shell
// ---------------------------------------------------------------------------

/// How the initial HTML document is produced on a fresh browser load.
#[derive(Debug)]
pub struct Shell {
    title: String,
    entry: String,
    dev: bool,
    dev_server: String,
    manifest_path: PathBuf,
    asset_base: String,
    /// Parsed once. A production build's manifest does not change while the
    /// process is running.
    manifest: OnceLock<BTreeMap<String, ManifestEntry>>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ManifestEntry {
    file: String,
    #[serde(default)]
    css: Vec<String>,
}

impl Shell {
    fn render(&self, page: &Value) -> String {
        let serialized = serde_json::to_string(page).unwrap_or_else(|_| "{}".to_owned());

        format!(
            "<!doctype html>\n\
             <html lang=\"en\">\n\
             <head>\n\
             <meta charset=\"utf-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
             <title>{title}</title>\n\
             {tags}\n\
             </head>\n\
             <body>\n\
             <div id=\"app\" data-page=\"{page}\"></div>\n\
             </body>\n\
             </html>\n",
            title = escape_html(&self.title),
            tags = self.tags(),
            page = escape_html(&serialized),
        )
    }

    /// The `<script>`/`<link>` tags that boot the client.
    fn tags(&self) -> String {
        if self.dev {
            // Vite's dev server serves the entry and its HMR client directly.
            // It sends permissive CORS headers, so loading these modules from
            // another port is fine — this is the same arrangement Laravel's
            // Vite plugin uses.
            return format!(
                "<script type=\"module\" src=\"{server}/@vite/client\"></script>\n\
                 <script type=\"module\" src=\"{server}/{entry}\"></script>",
                server = self.dev_server.trim_end_matches('/'),
                entry = self.entry,
            );
        }

        let manifest = self.manifest.get_or_init(|| {
            std::fs::read_to_string(&self.manifest_path)
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default()
        });

        let Some(entry) = manifest.get(&self.entry) else {
            // A missing manifest in a release build is a deployment mistake,
            // and a blank page with no explanation is the worst way to find
            // out. Say what is wrong, in the document itself.
            return format!(
                "<!-- luxid: no manifest entry for `{entry}` in `{path}`. \
                 Run your frontend build, or set `.dev(true)`. -->",
                entry = escape_html(&self.entry),
                path = escape_html(&self.manifest_path.display().to_string()),
            );
        };

        let base = self.asset_base.trim_end_matches('/');

        let mut tags: Vec<String> = entry
            .css
            .iter()
            .map(|href| format!("<link rel=\"stylesheet\" href=\"{base}/{href}\">"))
            .collect();

        tags.push(format!(
            "<script type=\"module\" src=\"{base}/{file}\"></script>",
            file = entry.file,
        ));

        tags.join("\n")
    }
}

/// Escape text for an HTML attribute or text node.
///
/// `data-page` carries user-controlled JSON into an attribute, so this is a
/// security boundary rather than a formatting nicety: without it, a todo
/// titled `"><script>` would execute.
fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());

    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }

    out
}

// ---------------------------------------------------------------------------
// The middleware
// ---------------------------------------------------------------------------

type SharedProps = Arc<dyn Fn(&HttpContext) -> Result<Value> + Send + Sync>;

/// Enables the Inertia protocol for a route group.
///
/// ```ignore
/// r.group("/", |r| {
///     r.middleware((Auth::session(), Inertia::new("resources/js/app.jsx")));
///
///     r.get("/", PagesController::home);
/// });
/// ```
pub struct Inertia {
    shell: Arc<Shell>,
    version: Option<String>,
    shared: Vec<SharedProps>,
}

impl Inertia {
    /// `entry` is the client entry point as Vite names it in its manifest,
    /// e.g. `"resources/js/app.jsx"`.
    pub fn new(entry: impl Into<String>) -> Self {
        Self {
            shell: Arc::new(Shell {
                title: "Luxid".to_owned(),
                entry: entry.into(),
                // Debug builds talk to the Vite dev server; release builds read
                // the built manifest. Predictable, and overridable with `.dev`.
                dev: cfg!(debug_assertions),
                dev_server: "http://localhost:5173".to_owned(),
                manifest_path: PathBuf::from("public/build/.vite/manifest.json"),
                asset_base: "/build".to_owned(),
                manifest: OnceLock::new(),
            }),
            version: None,
            shared: Vec::new(),
        }
    }

    fn shell_mut(&mut self) -> &mut Shell {
        Arc::get_mut(&mut self.shell).expect("the shell is not shared during construction")
    }

    /// The `<title>` of the HTML shell. Client-side title changes are the
    /// adapter's job; this is only the first paint.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.shell_mut().title = title.into();
        self
    }

    /// Serve assets from the Vite dev server (`true`) or a built manifest
    /// (`false`). Defaults to `cfg!(debug_assertions)`.
    pub fn dev(mut self, dev: bool) -> Self {
        self.shell_mut().dev = dev;
        self
    }

    pub fn dev_server(mut self, url: impl Into<String>) -> Self {
        self.shell_mut().dev_server = url.into();
        self
    }

    pub fn manifest(mut self, path: impl Into<PathBuf>) -> Self {
        self.shell_mut().manifest_path = path.into();
        self
    }

    /// URL prefix the built assets are served from. Must match the route the
    /// application registers with `Router::static_files`.
    pub fn asset_base(mut self, base: impl Into<String>) -> Self {
        self.shell_mut().asset_base = base.into();
        self
    }

    /// Asset version. When the client's copy differs, it hard-reloads instead
    /// of navigating — which is how a deploy reaches tabs that are already open
    /// and would otherwise keep requesting a bundle that no longer exists.
    ///
    /// A build hash or release tag is the usual value.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Props merged into every page rendered by this group.
    ///
    /// The authenticated user is the canonical example: every page wants it,
    /// and no page should have to remember to include it.
    ///
    /// ```ignore
    /// Inertia::new("resources/js/app.jsx")
    ///     .share(|ctx| Ok(json!({ "auth": { "id": ctx.auth.try_identity().map(|i| i.subject()) } })))
    /// ```
    pub fn share(
        mut self,
        props: impl Fn(&HttpContext) -> Result<Value> + Send + Sync + 'static,
    ) -> Self {
        self.shared.push(Arc::new(props));
        self
    }
}

impl Middleware for Inertia {
    fn handle<'a>(&'a self, mut ctx: HttpContext, next: Next) -> BoxFuture<'a, Result<Response>> {
        Box::pin(async move {
            let is_inertia = ctx.request.header("x-inertia").is_some();
            let is_get = ctx.request.method().as_str() == "GET";
            let url = full_path(&ctx);

            // ---- asset version check --------------------------------------
            //
            // Only on GET. A stale version on a write would discard the user's
            // input; Inertia's client handles that case itself.
            if is_inertia && is_get {
                let presented = ctx.request.header("x-inertia-version").unwrap_or("");
                let current = self.version.as_deref().unwrap_or("");

                if presented != current {
                    // 409 tells the client to abandon the XHR and do a full
                    // browser load of this location, picking up new assets.
                    //
                    // Built by hand rather than with `.no_content()`, which
                    // would reset the status to 204.
                    return Ok(Response::default()
                        .status(409)
                        .header("x-inertia-location", url));
                }
            }

            // ---- shared props ---------------------------------------------
            //
            // Computed here, while the context still exists. Errors flashed by
            // the *previous* request are read from the session and shared, which
            // is what makes the redirect-back flow deliver them to the page.
            let mut shared = Map::new();

            let flashed_errors: Value = ctx
                .session
                .flashed(ERRORS)
                .unwrap_or(None)
                .unwrap_or_else(|| json!({}));

            shared.insert(ERRORS.to_owned(), flashed_errors);

            for provider in &self.shared {
                if let Value::Object(map) = provider(&ctx)? {
                    shared.extend(map);
                }
            }

            // ---- remember where a form was submitted from -----------------
            //
            // Stored on GETs so a later POST knows where to bounce back to.
            // Taken from the session rather than the `Referer` header, which is
            // client-controlled and would make this an open redirect.
            let session = ctx.session.clone();
            let has_session = session.is_active();

            if is_get && !is_inertia_partial(&ctx) && has_session {
                let _ = session.put(PREVIOUS_URL, &url);
            }

            let back = session
                .get::<String>(PREVIOUS_URL)
                .unwrap_or(None)
                .unwrap_or_else(|| "/".to_owned());

            ctx.inertia = InertiaRequest {
                inner: Some(Arc::new(RequestState {
                    is_inertia,
                    url,
                    version: self.version.clone(),
                    shared: Value::Object(shared),
                    shell: self.shell.clone(),
                    only: header_list(&ctx, "x-inertia-partial-data"),
                    except: header_list(&ctx, "x-inertia-partial-except"),
                    partial_component: ctx
                        .request
                        .header("x-inertia-partial-component")
                        .map(str::to_owned),
                })),
            };

            // ---- run the action -------------------------------------------
            match next.run(ctx).await {
                Ok(response) => Ok(response),

                // The reason this middleware exists. A validation failure on an
                // Inertia request is not an error document — it is a bounce back
                // to the form with the errors attached.
                Err(Error::Validation(errors)) if is_inertia => {
                    if !has_session {
                        return Err(Error::internal(
                            "Inertia needs a session to report validation errors. Add \
                             `Auth::session()` outside `Inertia::new(..)` on this group, \
                             and bind a `SessionStore` in `providers()`.",
                        ));
                    }

                    // Inertia's `errors` prop is one message per field; Luxid
                    // collects every message. Taking the first matches what the
                    // client adapters render and what every other server adapter
                    // sends.
                    let first: Map<String, Value> = errors
                        .fields()
                        .filter_map(|(field, messages)| {
                            messages.first().map(|m| (field.clone(), json!(m)))
                        })
                        .collect();

                    session.flash(ERRORS, Value::Object(first))?;

                    // 303 rather than 302: after a PUT or DELETE, a 302 would
                    // have the browser repeat the method against the new URL.
                    Response::default().redirect(back)
                }

                Err(other) => Err(other),
            }
        })
    }
}

/// Path plus query, which is what Inertia's `url` field expects.
fn full_path(ctx: &HttpContext) -> String {
    let uri = ctx.request.uri();

    match uri.query() {
        Some(query) => format!("{}?{}", uri.path(), query),
        None => uri.path().to_owned(),
    }
}

fn is_inertia_partial(ctx: &HttpContext) -> bool {
    ctx.request.header("x-inertia-partial-component").is_some()
}

fn header_list(ctx: &HttpContext, name: &str) -> Option<Vec<String>> {
    ctx.request.header(name).map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect()
    })
}
