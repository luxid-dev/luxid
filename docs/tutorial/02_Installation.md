# 02 — Installation

## Rust

Luxid needs **Rust 1.94 or newer** and uses edition 2024. If you do not have
Rust:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Check what you have:

```sh
rustc --version
```

If it prints something older than 1.94:

```sh
rustup update stable
```

## The `luxid` command

Luxid ships a command-line tool that creates projects and generates code:

```sh
cargo install luxid-cli
```

That installs a binary called `luxid` into `~/.cargo/bin`. Verify it:

```sh
luxid --help
```

If the shell cannot find it, `~/.cargo/bin` is not on your `PATH`. Add it:

```sh
# bash / zsh — in ~/.bashrc or ~/.zshrc
export PATH="$HOME/.cargo/bin:$PATH"

# fish — in ~/.config/fish/config.fish
fish_add_path ~/.cargo/bin
```

## A database — or not

Luxid defaults to **SQLite**, which needs nothing installed: the database is a
file in your project directory. You can complete this entire course without
setting up anything.

When you want Postgres later, it is one environment variable. Chapter 11 covers
it.

## Two commands, two places

This trips people up, so it is worth stating early.

**`luxid`** — the tool you just installed. It creates projects and generates
files. It only touches the filesystem.

```sh
luxid new my-app
luxid make:model Post -a
```

**`cargo run --`** — your *application's own* command line. It runs migrations,
prints routes, serves.

```sh
cargo run -- migrate
cargo run -- routes
cargo run                  # serve
```

Why two? Because `migrate` and `routes` need to know about *your* migrations and
*your* routes — and those are Rust types that live in your crate. No external
program can see them. So those commands live inside your application's binary,
wired up by one line in `main.rs`.

Scaffolding is different: creating files needs no knowledge of your code, so it
lives in the standalone tool.

## Optional: a faster linker

Rust spends a surprising amount of build time linking. If you install
[mold](https://github.com/rui314/mold), your rebuilds get noticeably faster:

```sh
# Arch
sudo pacman -S mold
# Debian / Ubuntu
sudo apt install mold
```

Every project `luxid new` creates includes a `.cargo/config.toml` with the mold
setting **commented out**. Uncomment it once mold is installed. It ships
disabled because a project that requires mold to build is a project that fails
on any machine without it — including your colleagues'.

## Checking it works

```sh
luxid new hello
cd hello
cargo run
```

The first build takes several minutes — Luxid pulls in an HTTP stack and an ORM,
and they compile once. Subsequent builds take seconds.

When it finishes:

```
luxid listening on http://127.0.0.1:3000
```

In another terminal:

```sh
curl localhost:3000/api/health
```

```json
{"status":"ok"}
```

That is a working Luxid application. The next chapter takes it apart.

---

Previous: [01 — Introduction](01_Introduction.md) · Next: [03 — Your First App](03_Your_First_App.md)
