# 10 — Configuration

Configuration is anything that changes between your laptop and production: a
database URL, a secret key, a page size.

## Two layers

Luxid reads `luxid.toml`, then lets **environment variables override it**. That
split follows the usual convention:

- **`luxid.toml`** holds what is true for everyone. It is committed.
- **The environment** holds what is true for this deployment. It is not.

```toml
# luxid.toml
[app]
name = "blog"
per_page = 20

[database]
strict_relations = true
```

```sh
# .env — not committed
DATABASE_URL=postgres://localhost/blog
APP_KEY=a-real-secret
```

## Keys are forgiving

A nested TOML table flattens to a dotted key, and separators and case do not
matter:

```
[database]              →  database.strict_relations
strict_relations = true →  DATABASE_STRICT_RELATIONS
                        →  database_strict_relations
```

All three spellings are **the same key**. So the environment override for
`app.per_page` is `APP_PER_PAGE`, without you having to look up a mapping.

## Reading it

Any action, middleware, or anything with a context:

```rust
async fn index(ctx: HttpContext) -> Result<Response> {
    let name: String = ctx.config.get("app.name")?;
    let per_page: u32 = ctx.config.get_or("app.per_page", 20)?;

    ctx.response.ok(json!({ "app": name, "per_page": per_page }))
}
```

| Method | Behaviour |
|---|---|
| `get::<T>(key)` | Required. Missing is an error naming the environment variable. |
| `try_get::<T>(key)` | Optional. Returns `Option<T>`. |
| `get_or(key, default)` | Uses the default when **absent**. |
| `raw(key)` | The unparsed string. |
| `has(key)` | Whether it is set. |

## Absent and malformed are different

This is worth internalising:

```rust
let per_page: u32 = ctx.config.get_or("app.per_page", 20)?;
```

- Key **absent** → you get `20`.
- Key present but set to `"twenty"` → **an error**, not `20`.

Silently falling back on a malformed value would hide a typo until someone
wondered why their setting had no effect. The default covers "you did not say",
not "you said something I could not read".

## Missing keys tell you the fix

```rust
let key: String = ctx.config.get("app.key")?;
```

If it is not set, your logs get:

```
configuration key `app.key` is not set. Add it to luxid.toml, or set `APP_KEY`.
```

The client gets a redacted `500` — configuration keys can be revealing.

## Where configuration is loaded

In `src/app.rs`:

```rust
pub async fn build() -> luxid::Result<App> {
    let config = Config::load("luxid.toml")?;

    // ...

    Ok(App::new().config(config).routes(crate::routes::register))
}
```

`Config::load` reads the file if it exists — a missing file is **not** an error,
since an application configured entirely by environment is perfectly ordinary —
and then layers the environment over it.

## Prefer a typed struct for real settings

`Config` is a string map with typed reads. That is fine for a handful of values,
but for anything your application depends on, parse it **once at boot** into a
struct and register that:

```rust
pub struct Settings {
    pub per_page: u32,
    pub app_key: String,
}

impl Settings {
    pub fn load(config: &Config) -> luxid::Result<Self> {
        Ok(Self {
            per_page: config.get_or("app.per_page", 20)?,
            app_key: config.get("app.key")?,
        })
    }
}
```

```rust
let settings = Settings::load(&config)?;

Ok(App::new()
    .config(config)
    .providers(Providers::new().singleton(move |_| settings.clone()))
    .routes(crate::routes::register))
```

Two things improve. A missing or malformed value now fails **at startup**
rather than on whichever request first reads it. And actions get a real struct:

```rust
let settings = ctx.services.get::<Settings>()?;
settings.per_page      // a u32, already validated
```

Use `ctx.config` for one-off reads and for building that struct. Use the struct
for everything else.

## Secrets

Never put a secret in `luxid.toml` — it is committed. Use the environment:

```sh
# .env, gitignored
APP_KEY=...
DATABASE_URL=postgres://user:password@host/db
```

`luxid new` gitignores `.env` and writes a `.env.example` showing which
variables exist without their values. Keep that habit: the example file is how
the next person knows what to set.

---

Previous: [09 — Services](09_Services.md) · Next: [11 — Database and Migrations](11_Database_and_Migrations.md)
