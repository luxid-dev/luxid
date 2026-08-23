# 01 — Introduction

## What Luxid is

Luxid is a web framework for Rust that takes its shape from Laravel and, more
directly, from AdonisJS. If you have used either, most of this will feel
familiar. If you have not, that is fine — this course assumes you have not.

The pitch is short: **Rust's performance and safety, without Rust's usual web
boilerplate.** You should be able to describe a resource and get a working,
documented, tested API out the other side.

Underneath, Luxid runs on [salvo](https://salvo.rs), a fast HTTP library. You
will not see salvo anywhere in your code. That is deliberate — the substrate is
sealed off so the framework can present one consistent surface.

## Who this is for

Someone who knows Rust reasonably well and wants to build a web service. You
should be comfortable with:

- structs, enums, traits, and `impl` blocks
- `Result<T, E>` and the `?` operator
- `async fn` and `.await`

You do **not** need to know salvo, SeaORM, tokio internals, or any other
framework. Each is introduced when it first matters.

## The four ideas

Almost everything in Luxid follows from four decisions. Learn these now and the
rest of the framework will feel predictable rather than arbitrary.

### 1. One context, owned

Every controller action takes exactly one argument:

```rust
async fn index(ctx: HttpContext) -> Result<Response> {
    ctx.response.ok(json!({ "hello": "world" }))
}
```

`HttpContext` carries everything the request needs — the request itself, a
response builder, route parameters, the authenticated user, the database, your
services, configuration, the session. There is no second signature to learn, no
set of "extractors" to memorise, and no way to get the argument list wrong.

Frameworks that use extractors ask you to write `async fn index(State(db):
State<Db>, Query(page): Query<Page>)` and, when you get it slightly wrong, hand
you a page of trait-bound errors. Luxid trades a little magic for signatures
that cannot fail to compile in confusing ways.

### 2. Errors carry their own status code

There is one error type, and each of its variants already knows what HTTP
response it should become:

```rust
async fn show(ctx: HttpContext) -> Result<Response> {
    let post = Post::find_or_fail(ctx.params.get::<i64>("id")?).await?;
    ctx.response.ok(post)
}
```

If that row does not exist, the client gets a well-formed `404` with a JSON
body — and there is no error handling in the action at all. The `?` did it.
This is the single biggest reason Luxid controllers stay short.

### 3. Convention, but visible

Luxid generates code for you: models, migrations, controllers, routes. What it
generates is **ordinary code in your project**, which you can read, edit, and
delete.

Some frameworks discover your routes by scanning the binary at startup. Luxid
does not. Your routes are a function you can read:

```rust
pub fn register(r: &mut Router) {
    r.group("/api", |r| {
        r.get("/health", controllers::health_controller::HealthController::show);
        r.resource("/posts", controllers::posts_controller::PostsController);
    });
}
```

When a route 404s, you can find out why by reading that file, or by running
`cargo luxid routes`. Nothing is hidden.

### 4. The mistakes should be loud

Luxid tries to turn quiet bugs into loud ones:

- Reading a database relation you forgot to load is an **error** in development,
  naming the fix — so an N+1 query becomes a failing test rather than a slow
  production endpoint.
- A service you forgot to register fails **at startup**, naming the type, rather
  than on the first request that needs it.
- A validation rule that needs the database runs in the same pass as the rest,
  so the client gets every problem at once rather than one per round trip.

## What a Luxid app looks like

```
my-app/
├── luxid.toml            configuration
├── migration/            schema changes over time
└── src/
    ├── main.rs           four lines
    ├── app.rs            assembling the application
    ├── routes.rs         the routing table
    ├── controllers/      what happens per endpoint
    ├── models/           your behaviour on database rows
    ├── entities/         generated from the database schema
    ├── validators/       input rules
    ├── policies/         permission rules
    ├── services/         your own shared objects
    ├── middleware/       code that runs around requests
    ├── factories/        test data
    └── seeders/          development data
```

If you have used Laravel, this is `app/Http/Controllers`, `app/Models`,
`database/migrations` under different names. If you have not, each directory
gets its own chapter.

## What Luxid is not

Being honest about this saves you time later.

- **It is not stable.** This is 0.1.x. The API will change.
- **It is API-first.** Luxid renders JSON. There is no template engine and no
  asset pipeline yet.
- **It does not do background jobs, email, or caching yet.** Those are planned.
- **It has one data layer.** Luxid uses SeaORM underneath. You can drop down to
  raw SeaORM whenever you need to, but you cannot swap in Diesel.

If you need server-rendered HTML today, or a job queue, Luxid is not ready for
you yet. If you are building a JSON API, read on.

---

Next: [02 — Installation](02_Installation.md)
