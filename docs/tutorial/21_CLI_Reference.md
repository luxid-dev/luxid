# 21 — CLI Reference

Two command lines, in two places. Chapter 02 explained why; this is the full
list.

## `luxid` — the standalone tool

Installed with `cargo install luxid-cli`. Creates files; knows nothing about
your code.

### `luxid new <name>`

Creates a project.

```sh
luxid new blog
luxid new blog --luxid-path /path/to/luxid    # depend on a local checkout
```

`--luxid-path` is for working on the framework itself. Ordinary projects do not
need it.

The name becomes the crate name, normalised — `my-app` gives a crate called
`my_app`.

### `luxid make:model <Name>`

Generates a model and, with flags, everything around it.

```sh
luxid make:model Post          # model + entity
luxid make:model Post -m       # + migration
luxid make:model Post -mc      # + resource controller
luxid make:model Post -mfsc    # + factory + seeder + controller
luxid make:model Post -a       # everything
```

| Flag | Generates |
|---|---|
| `-m` | migration |
| `-f` | factory |
| `-s` | seeder |
| `-c` | API resource controller, and registers its routes |
| `-a` | all of the above, plus a policy and form requests |

Flags combine: `-mfsc` is four of them.

`-a` is what you want most of the time. There is no standalone flag for policies
or form requests — `-a` produces them.

`-c` generates an **API** resource controller (`index show store update
destroy`, no `create`/`edit` form actions) and adds one `r.resource(...)` line to
`routes.rs`.

Names are normalised, so `Post`, `post`, and `user_profile` all work. Plurals are
derived — `Category` becomes the table `categories`. The rules are simple and
will get irregular nouns wrong; override with `#[luxid(name = "...")]` on the
entity when they do.

**Nothing is overwritten.** If any target file exists, the command writes nothing
at all and says which clashed — a half-applied generator is worse than one that
declined.

## `cargo luxid` — your application

These need your routes, migrations, and services, so they live in your binary.

`cargo luxid` is a cargo alias, written into `.cargo/config.toml` by
`luxid new`:

```toml
[alias]
luxid = "run --"
```

Cargo expands it before dispatch, so `cargo luxid migrate` and
`cargo run -- migrate` are the same command and either will do. If you are
adding Luxid to a project that already existed, copy those two lines across to
get the shorter form.

### Serving

```sh
cargo run              # serve (the default)
cargo luxid serve      # the same thing
```

Address comes from `LUXID_ADDR`, then `PORT`, then `127.0.0.1:3000`.

### Migrations

```sh
cargo luxid migrate                  # apply everything pending
cargo luxid migrate --steps 1        # apply at most one
cargo luxid migrate:rollback         # undo the last
cargo luxid migrate:rollback --steps 3
cargo luxid migrate:status           # what has run
cargo luxid migrate:fresh --force    # drop everything and rebuild
```

`migrate:fresh` requires `--force`, because destroying every table should not
follow from a mistyped command in the wrong shell.

### Schema sync

```sh
cargo luxid db:sync
cargo luxid db:sync --dry-run
```

Reads the live database and refreshes the field lists in your entities and
factories — but only what lies between the `// <luxid:fields>` markers. Anything
outside them survives.

Run it after every migration.

### Inspecting

```sh
cargo luxid routes
```

```
GET     /api/posts       PostsController::index    [1 middleware]
POST    /api/posts       PostsController::store    [1 middleware]
GET     /api/posts/{id}  PostsController::show     [1 middleware]
```

The first thing to check when an endpoint behaves unexpectedly.

```sh
cargo luxid openapi
cargo luxid openapi --pretty --title "Blog API" --version 1.0.0
```

## Cargo commands worth knowing

```sh
cargo test                  # the suite
cargo clippy --all-targets  # lints
cargo fmt --all             # formatting
cargo build --release       # an optimised binary
```

## A typical session

```sh
luxid new blog && cd blog

luxid make:model Post -a
# edit migration/src/m..._create_posts.rs to add columns
cargo luxid migrate
cargo luxid db:sync

cargo luxid routes
cargo run
```

## When something is not working

| Symptom | Check |
|---|---|
| 404 on a route you added | `cargo luxid routes` — is it registered? |
| "file not found for module" | You forgot `pub mod ...;` in the parent `mod.rs` |
| "no database connection is in scope" | `WithDatabase` is missing from `app.rs` |
| "no provider bound for `X`" | Register it in `providers()` |
| "the `x` relation was not loaded" | Add `.with("x")` to the query |
| "no session is active" | Add `.middleware(Auth::session())` |
| Column not found after a migration | `cargo luxid db:sync` |

Luxid's error messages generally name the fix. When one does not, that is worth
reporting as a bug.

---

Previous: [20 — Testing](20_Testing.md) · Next: [22 — Project: an Auth API](22_Project_Auth_App.md)
