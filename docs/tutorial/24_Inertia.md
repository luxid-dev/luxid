# 24 — Views with Inertia

Everything so far returns JSON. This chapter returns pages.

Luxid has no templating engine, and is not getting one. It speaks the
[Inertia.js](https://inertiajs.com) protocol instead: your controllers return a
component name and some props, and a React, Vue or Svelte client renders them.
You write server-side routing and controllers exactly as you already do, and get
a modern frontend without building an API for your own frontend to consume.

## Why a protocol instead of templates

A templating engine gives you a second view layer that still cannot do
interactivity — you end up writing templates *and* sprinkling JavaScript. Inertia
gives you one.

And the client half already exists. `@inertiajs/react` and its siblings are
maintained by someone else; Luxid only implements the server half, which is small
enough to fit in one file.

What you do *not* get, compared with a separate SPA and API:

- no API client to write or keep in step
- no CORS — the page and its data come from the same origin
- no token in `localStorage` — the session cookie you already have is enough
- no client-side route table that can disagree with the server's

## Starting a new app

```sh
luxid new blog
```

```
What are you building?

  1) API only          JSON endpoints. No frontend, no Node. (default)
  2) SSR with Inertia  Server-driven pages rendered by React, Vue or Svelte.

  [1-2] 2

Which client framework?

  1) React   The default. (default)
  2) Vue     Vue 3, script setup.
  3) Svelte  Svelte 5, runes.

  [1-3] 1
```

Non-interactively — and this is what happens automatically when stdin is not a
terminal, so scripts and CI never hang:

```sh
luxid new blog --stack inertia --client react
```

Two terminals from then on:

```sh
npm install && npm run dev      # Vite on :5173, for the JS modules
cargo run                       # the app on :3000 — this is the one you open
```

Open `http://127.0.0.1:3000`. Vite is not a proxy and you never visit it
directly; it only serves the JavaScript the page asks for, and hot-reloads it.

## Rendering a page

```rust
use luxid::prelude::*;
use serde_json::json;

pub struct PostsController;

#[luxid::controller]
impl PostsController {
    async fn index(ctx: HttpContext) -> Result<Response> {
        let posts = Post::query().order_by_desc(Post::id).paginate(1, 20).await?;

        ctx.inertia("Posts/Index", json!({ "posts": posts }))
    }
}
```

`"Posts/Index"` resolves to `resources/js/Pages/Posts/Index.jsx`. The props
arrive as that component's props:

```jsx
export default function Index({ posts }) {
  return <ul>{posts.data.map((p) => <li key={p.id}>{p.title}</li>)}</ul>
}
```

No `fetch`, no loading state, no `useEffect`. The data came down with the page.

## One action, two audiences

`ctx.inertia` returns different things depending on how it was asked:

| Request | Response |
|---|---|
| a fresh browser load | an HTML shell with the page in `data-page` |
| a link click (`X-Inertia: true`) | JSON `{component, props, url, version}` |

The action does not know which, and does not need to.

## Routing

```rust
pub fn register(r: &mut Router) {
    r.static_files("/build", "public/build");

    r.group("/", |r| {
        r.middleware(Auth::session());
        r.middleware(Inertia::new("resources/js/app.jsx"));

        r.get("/posts", PostsController::index);
    });

    r.group("/api", |r| {
        // No Inertia middleware here — see below.
    });
}
```

**`Auth::session()` must be outside `Inertia`.** This is not stylistic. The
session guard writes the session back with `next.run(ctx).await?`, so it never
sees an `Err`. The Inertia middleware turns a validation failure into a redirect
*before* that happens, which is what lets the flashed errors survive. Reversed,
the errors are written and then silently dropped.

## Forms, and the one thing that surprises everyone

Inertia is built on post-redirect-get. A failed form does not render an error
document — it bounces back to the page it came from, and the errors arrive as a
prop.

So on an Inertia route, a validation failure is **not** a 422:

```rust
async fn store(ctx: HttpContext) -> Result<Response> {
    let input = ctx.request.validate::<StorePost>().await?;   // <- 303 on failure
    ...
}
```

The middleware flashes the errors to the session and redirects back. The next
page render receives them as the shared `errors` prop:

```jsx
export default function Create({ errors }) {
  return (
    <form>
      <input name="title" />
      {errors.title && <span>{errors.title}</span>}
    </form>
  )
}
```

`errors` is always present — `{}` when there is nothing wrong — so you never
have to guard for its absence.

**Nothing about `Error` changed.** A route *without* the Inertia middleware still
answers a validation failure with `422 application/problem+json`. The same
action, the same validator and the same `ctx.request.validate::<T>()` call serve
both; which rendering you get depends on which group the route is in. That is why
the `/api` group in the scaffold is left alone.

Note also that `errors` is one message per field, matching what the client
adapters render — while the 422 body keeps every message Luxid found.

## Shared props

Props every page needs, declared once:

```rust
Inertia::new("resources/js/app.jsx")
    .share(|ctx| {
        Ok(json!({
            "auth": { "user": ctx.auth.try_identity().map(|i| i.subject()) }
        }))
    })
```

A page prop of the same name wins, so a page can override a shared value.

## Flash messages

`Session::flash` is ordinary Luxid, usable with or without Inertia:

```rust
ctx.session.flash("notice", "Post published")?;
```

Readable on the next request only, then discarded — which is what stops a stale
message rendering on an unrelated page. Surface it with `.share(..)`:

```rust
.share(|ctx| Ok(json!({ "notice": ctx.session.flashed::<String>("notice")? })))
```

## Partial reloads

The client can ask for a subset of the props it already knows the names of, and
Luxid filters them out of the response:

```jsx
router.reload({ only: ['posts'] })
```

Honoured only when the requested component matches the one being rendered —
otherwise the page would arrive missing most of its data.

## Assets, development and production

A debug build talks to the Vite dev server. A release build reads
`public/build/.vite/manifest.json` for the hashed filenames, and
`r.static_files("/build", "public/build")` serves them.

```sh
npm run build
cargo build --release
```

Override the heuristic with `.dev(true)` or `.dev(false)` when you need to.

## Asset versioning

```rust
Inertia::new("resources/js/app.jsx").version(env!("CARGO_PKG_VERSION"))
```

When the client's copy of the version differs from the server's, it stops
navigating and does a full browser load instead. That is how a deploy reaches a
tab that has been open all afternoon and would otherwise keep asking for a
bundle that no longer exists.

## Adding Inertia to an existing app

Nothing here is special to a scaffolded app. Bind a `SessionStore`, add the two
middleware to a group, add `Inertia::new(..)` and a `package.json`. The
`luxid new` scaffold is a convenience, not a requirement — and an API-only app
can grow a UI later without regenerating anything.

---

Previous: [23 — Project: a Todo API](23_Project_Todo_App.md) · [Back to the index](README.md)
