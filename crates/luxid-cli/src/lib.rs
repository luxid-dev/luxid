//! The application's command line.
//!
//! This lives *inside* the app binary rather than in a standalone `luxid`
//! executable, because the things these commands operate on — the route table,
//! the migration list, the container — are types in the application's own
//! crate. No external binary can see them.
//!
//! ```ignore
//! #[tokio::main]
//! async fn main() -> luxid::Result<()> {
//!     luxid::cli::run::<Migrator>(build_app()).await
//! }
//! ```
//!
//! Scaffolding commands (`new`, `make:model`) are the part that *can* be
//! standalone, since they only touch the filesystem.

pub mod naming;
pub mod prompt;
pub mod scaffold;

use clap::{Parser, Subcommand};
use luxid_core::error::{Error, Result};
use luxid_core::{App, RouteInfo};
use luxid_orm::Db;
use sea_orm_migration::MigratorTrait;

#[derive(Parser)]
#[command(
    name = "app",
    about = "A Luxid application",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the application (the default when no command is given).
    Serve,

    /// Apply pending migrations.
    Migrate {
        /// Apply at most this many.
        #[arg(long)]
        steps: Option<u32>,
    },

    /// Roll back applied migrations.
    #[command(name = "migrate:rollback")]
    MigrateRollback {
        #[arg(long, default_value_t = 1)]
        steps: u32,
    },

    /// Drop everything and migrate from scratch.
    #[command(name = "migrate:fresh")]
    MigrateFresh {
        /// Required: this destroys data.
        #[arg(long)]
        force: bool,
    },

    /// Show which migrations have run.
    #[command(name = "migrate:status")]
    MigrateStatus,

    /// Refresh generated field lists from the live database schema.
    ///
    /// Rewrites only what lies between the `<luxid:fields>` markers, so rules
    /// and overrides you wrote outside them survive.
    #[command(name = "db:sync")]
    DbSync {
        /// Report what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Print the routing table.
    Routes,

    /// Emit the OpenAPI 3.1 document.
    Openapi {
        #[arg(long, default_value = "API")]
        title: String,

        #[arg(long, default_value = "0.1.0")]
        version: String,

        /// Indent the output. Off by default so it pipes cleanly.
        #[arg(long)]
        pretty: bool,
    },
}

/// Parse the process arguments and run the matching command.
pub async fn run<M: MigratorTrait>(app: App) -> Result<()> {
    dispatch::<M>(app, Cli::parse().command.unwrap_or(Command::Serve)).await
}

async fn dispatch<M: MigratorTrait>(app: App, command: Command) -> Result<()> {
    match command {
        Command::Serve => app.run().await,

        Command::Routes => {
            print!("{}", render_routes(&app.route_table()));
            Ok(())
        }

        Command::Openapi {
            title,
            version,
            pretty,
        } => {
            let document = app.openapi(&title, &version);

            let rendered = if pretty {
                serde_json::to_string_pretty(&document)
            } else {
                serde_json::to_string(&document)
            }
            .map_err(|err| Error::internal(format!("could not render the document: {err}")))?;

            println!("{rendered}");
            Ok(())
        }

        Command::Migrate { steps } => {
            let db = database(app)?;

            match steps {
                Some(steps) => db.migrate_steps::<M>(steps).await?,
                None => db.migrate::<M>().await?,
            }

            println!("migrations applied");
            Ok(())
        }

        Command::MigrateRollback { steps } => {
            database(app)?.rollback::<M>(steps).await?;
            println!("rolled back {steps} migration(s)");
            Ok(())
        }

        Command::MigrateFresh { force } => {
            // Dropping every table is not something to do because a command was
            // mistyped in the wrong shell.
            if !force {
                return Err(Error::internal(
                    "migrate:fresh drops every table. Re-run with --force if that is what you want.",
                ));
            }

            database(app)?.migrate_fresh::<M>().await?;
            println!("database rebuilt");
            Ok(())
        }

        Command::DbSync { dry_run } => {
            let db = database(app)?;
            let tables = db.tables().await?;

            let mut touched = 0;

            for table in &tables {
                let model = crate::naming::to_snake(&singular(&table.name));

                // The entity must be refreshed too, or the factory below ends
                // up referencing columns the entity does not declare.
                // Attributes the user wrote on a field must survive the
                // regeneration — `#[serde(skip_serializing)]` on a password hash
                // silently becoming "sent to every client" is not an acceptable
                // outcome of running a sync command.
                let carried = std::fs::read_to_string(format!("src/entities/{}.rs", table.name))
                    .map(|source| crate::scaffold::field_attributes(&source))
                    .unwrap_or_default();

                let entity_body: String = table
                    .columns
                    .iter()
                    .map(|column| {
                        let ty = if column.nullable {
                            format!("Option<{}>", column.rust_type())
                        } else {
                            column.rust_type().to_owned()
                        };

                        let mut lines: Vec<String> =
                            carried.get(&column.name).cloned().unwrap_or_default();

                        if column.primary_key {
                            lines.insert(0, "#[sea_orm(primary_key)]".to_owned());
                        }

                        lines.push(format!("pub {}: {ty},", column.name));
                        lines.join("\n")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                let factory_body: String = table
                    .required_columns()
                    .map(|column| format!("{}: {},", column.name, column.sample()))
                    .collect::<Vec<_>>()
                    .join("\n");

                for (path, body) in [
                    (format!("src/entities/{}.rs", table.name), entity_body),
                    (format!("src/factories/{model}_factory.rs"), factory_body),
                ] {
                    let path = std::path::PathBuf::from(path);

                    let Ok(source) = std::fs::read_to_string(&path) else {
                        continue;
                    };

                    let Some(updated) = crate::scaffold::refresh_fields(&source, &body) else {
                        println!("  {}: no <luxid:fields> markers, skipped", path.display());
                        continue;
                    };

                    if updated == source {
                        continue;
                    }

                    touched += 1;

                    if dry_run {
                        println!("  would update {}", path.display());
                    } else {
                        std::fs::write(&path, updated).map_err(|err| {
                            Error::internal(format!("could not write {}: {err}", path.display()))
                        })?;
                        println!("  updated {}", path.display());
                    }
                }
            }

            println!(
                "{} table(s) read, {touched} file(s) {}",
                tables.len(),
                if dry_run { "would change" } else { "changed" }
            );
            Ok(())
        }

        Command::MigrateStatus => {
            let states = database(app)?.migration_status::<M>().await?;

            if states.is_empty() {
                println!("no migrations");
                return Ok(());
            }

            for state in states {
                let mark = if state.applied { "applied" } else { "pending" };
                println!("{mark:>8}  {}", state.name);
            }
            Ok(())
        }
    }
}

/// Table names are plural; factories are named after the singular model.
///
/// Mirrors the rule `#[derive(Model)]` uses, so the two agree on which factory
/// belongs to which table.
fn singular(table: &str) -> String {
    let (head, last) = match table.rsplit_once('_') {
        Some((head, last)) => (Some(head), last),
        None => (None, table),
    };

    let singular = if let Some(stem) = last.strip_suffix("ies") {
        format!("{stem}y")
    } else if last.ends_with("sses") || last.ends_with("shes") || last.ends_with("ches") {
        last.trim_end_matches("es").to_owned()
    } else if last.ends_with("ss") {
        last.to_owned()
    } else if let Some(stem) = last.strip_suffix('s') {
        stem.to_owned()
    } else {
        last.to_owned()
    };

    match head {
        Some(head) => format!("{head}_{singular}"),
        None => singular,
    }
}

/// The database the app's providers bind.
fn database(app: App) -> Result<std::sync::Arc<Db>> {
    let (_, services) = app.into_parts();

    services.get::<Db>().map_err(|_| {
        Error::internal(
            "this command needs a database, but no `Db` is bound. \
             Add `.singleton(move |_| db.clone())` to `providers()`.",
        )
    })
}

/// Render the routing table as aligned columns.
pub fn render_routes(routes: &[RouteInfo]) -> String {
    if routes.is_empty() {
        return "no routes registered\n".to_owned();
    }

    let method_width = routes
        .iter()
        .map(|route| route.method.as_str().len())
        .max()
        .unwrap_or(6);
    let path_width = routes
        .iter()
        .map(|route| route.path.len())
        .max()
        .unwrap_or(4);

    // Pad the action column too, or the middleware column comes out ragged
    // whenever action names differ in length — which is always.
    let has_middleware = routes.iter().any(|route| route.middleware > 0);
    let action_width = if has_middleware {
        routes.iter().map(|r| r.action.len()).max().unwrap_or(6)
    } else {
        0
    };

    let mut out = String::new();
    for route in routes {
        out.push_str(&format!(
            "{:<method_width$}  {:<path_width$}  {:<action_width$}",
            route.method.as_str(),
            route.path,
            route.action,
        ));

        if route.middleware > 0 {
            out.push_str(&format!("  [{} middleware]", route.middleware));
        }

        out.truncate(out.trim_end().len());
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxid_core::Method;

    fn route(method: Method, path: &str, action: &'static str, middleware: usize) -> RouteInfo {
        RouteInfo {
            method,
            path: path.to_owned(),
            action,
            middleware,
            operation: None,
        }
    }

    #[test]
    fn renders_aligned_columns() {
        let rendered = render_routes(&[
            route(Method::Get, "/api/v1/users", "UsersController::index", 2),
            route(Method::Post, "/api/v1/users", "UsersController::store", 2),
        ]);

        let lines: Vec<_> = rendered.lines().collect();

        assert_eq!(
            lines[0],
            "GET   /api/v1/users  UsersController::index  [2 middleware]"
        );
        assert_eq!(
            lines[1],
            "POST  /api/v1/users  UsersController::store  [2 middleware]"
        );
    }

    #[test]
    fn omits_the_middleware_column_when_there_is_none() {
        let rendered = render_routes(&[route(Method::Get, "/health", "Health::show", 0)]);
        assert_eq!(rendered.trim_end(), "GET  /health  Health::show");
    }

    #[test]
    fn says_so_when_nothing_is_registered() {
        assert_eq!(render_routes(&[]), "no routes registered\n");
    }
}
