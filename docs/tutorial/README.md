# The Luxid Tutorial

A course, not a reference. Read it in order and you will finish able to build
and ship a real Luxid application. Every code sample here is taken from working
code.

You need to know some Rust — structs, traits, `Result`, `async`/`await`. You do
**not** need to have used a web framework before. Where Luxid assumes an idea
(middleware, migrations, dependency injection), the chapter that needs it
explains it first.

## Part 1 — Getting going

| | |
|---|---|
| [01 Introduction](01_Introduction.md) | What Luxid is, and the four ideas the whole framework rests on |
| [02 Installation](02_Installation.md) | Rust, the `luxid` binary, your first project |
| [03 Your First App](03_Your_First_App.md) | A working endpoint in ten minutes |

## Part 2 — Handling requests

| | |
|---|---|
| [04 Routing](04_Routing.md) | Paths, groups, route parameters, resources |
| [05 Controllers](05_Controllers.md) | Actions, `HttpContext`, and how a request flows |
| [06 Requests and Responses](06_Requests_and_Responses.md) | Reading input, writing output |
| [07 Errors](07_Errors.md) | Why `?` is enough, and what your clients see |
| [08 Middleware](08_Middleware.md) | Running code around every request |

## Part 3 — The application

| | |
|---|---|
| [09 Services](09_Services.md) | The container: shared objects, done safely |
| [10 Configuration](10_Configuration.md) | `luxid.toml`, environment variables, `ctx.config` |

## Part 4 — Data

| | |
|---|---|
| [11 Database and Migrations](11_Database_and_Migrations.md) | Connecting, and changing your schema over time |
| [12 Models and Queries](12_Models_and_Queries.md) | Reading and writing rows |
| [13 Relations](13_Relations.md) | Linking models, and defeating the N+1 problem |
| [14 Scopes and Hooks](14_Scopes_and_Hooks.md) | Reusable query pieces, and lifecycle callbacks |
| [15 Validation](15_Validation.md) | Rejecting bad input, including rules that hit the database |

## Part 5 — Users

| | |
|---|---|
| [16 Authentication](16_Authentication.md) | Passwords, tokens, and who the request is |
| [17 Sessions](17_Sessions.md) | Cookie-backed state and browser logins |
| [18 Authorization](18_Authorization.md) | Deciding what a user may do |

## Part 6 — Shipping

| | |
|---|---|
| [19 OpenAPI](19_OpenAPI.md) | Documenting your API from the code |
| [20 Testing](20_Testing.md) | A test suite that stays fast and honest |
| [21 CLI Reference](21_CLI_Reference.md) | Every command, in one place |

## Part 7 — Build something

| | |
|---|---|
| [22 Project: an Auth API](22_Project_Auth_App.md) | Register, log in, protected routes — from scratch |
| [23 Project: a Todo API](23_Project_Todo_App.md) | Ownership, relations, filtering, a full test suite |

---

Two habits worth forming as you read:

**Run the code.** Every chapter's samples fit into the app you build in chapter
03. Typing them out and watching them fail teaches more than reading them.

**Read the error messages.** Luxid tries hard to make them tell you the fix —
which relation to eager-load, which service to bind, which environment variable
to set. When one does not, that is a bug worth reporting.
