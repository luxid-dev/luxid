//! The standalone `luxid` binary: scaffolding only.
//!
//! Runtime commands — `migrate`, `routes`, `openapi`, `serve` — live in the
//! application's own binary, because they operate on types in the application's
//! crate. These commands only touch the filesystem, so they can live out here.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use luxid_cli::naming::Names;
use luxid_cli::scaffold::{self, Dependency, ModelFlags};

/// Version the generated app depends on, when not pointed at a local checkout.
const LUXID_VERSION: &str = "0.1";

#[derive(Parser)]
#[command(name = "luxid", version, about = "Scaffolding for Luxid applications")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new application.
    New {
        /// Directory to create. Its name becomes the crate name.
        name: String,

        /// Depend on a local Luxid checkout instead of a published version.
        #[arg(long, value_name = "DIR")]
        luxid_path: Option<PathBuf>,
    },

    /// Generate a model and, with flags, the artefacts around it.
    ///
    /// Combine short flags: `-mc`, `-mfsc`. `-a` is everything.
    #[command(name = "make:model")]
    MakeModel {
        /// Model name, e.g. `User` or `user_profile`.
        name: String,

        /// Migration.
        #[arg(short = 'm')]
        migration: bool,

        /// Factory.
        #[arg(short = 'f')]
        factory: bool,

        /// Seeder.
        #[arg(short = 's')]
        seeder: bool,

        /// API resource controller, with its routes registered.
        #[arg(short = 'c')]
        controller: bool,

        /// Everything: migration, factory, seeder, policy, resource controller,
        /// and form requests.
        #[arg(short = 'a')]
        all: bool,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    match Cli::parse().command {
        Command::New { name, luxid_path } => {
            let root = PathBuf::from(&name);

            if root.exists() {
                return Err(format!("{name} already exists"));
            }

            let dependency = match luxid_path {
                Some(path) => {
                    let absolute = path
                        .canonicalize()
                        .map_err(|err| format!("{}: {err}", path.display()))?;

                    Dependency::Path(absolute.display().to_string())
                }
                None => Dependency::Version(LUXID_VERSION.to_owned()),
            };

            // Luxid is not on crates.io yet, so a version dependency produces a
            // project that cannot resolve. Say so rather than letting `cargo
            // build` deliver the news.
            if matches!(dependency, Dependency::Version(_)) {
                eprintln!(
                    "note: luxid {LUXID_VERSION} is not published yet, so this project will not \
                     build as generated.\n      Pass --luxid-path <checkout> to depend on a local \
                     copy of the framework."
                );
                eprintln!();
            }

            let plan = scaffold::new_app(&name, &dependency);
            let written = scaffold::write(&plan, &root).map_err(|err| err.to_string())?;

            report(&name, &written);

            // Only suggest it when it would actually work here.
            if which("mold") {
                println!();
                println!("mold detected — uncomment .cargo/config.toml for faster links.");
            }

            println!();
            println!("    cd {name}");
            println!("    cargo run              # serves on http://127.0.0.1:3000");
            println!("    cargo run -- routes");

            Ok(())
        }

        Command::MakeModel {
            name,
            migration,
            factory,
            seeder,
            controller,
            all,
        } => {
            let root = project_root()?;

            let flags = if all {
                ModelFlags::all()
            } else {
                ModelFlags {
                    migration,
                    factory,
                    seeder,
                    controller,
                    // Only `-a` produces these; there is no standalone flag.
                    policy: false,
                    requests: false,
                }
            };

            let names = Names::new(&name);
            let plan = scaffold::make_model(&names, flags, &timestamp());

            let written = scaffold::write(&plan, &root).map_err(|err| err.to_string())?;
            report(&names.model, &written);

            if flags.migration {
                println!();
                println!("Fill in the migration's columns, then:");
                println!("    cargo run -- migrate");
            }

            Ok(())
        }
    }
}

/// Whether a binary is on PATH.
fn which(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(binary).is_file()))
        .unwrap_or(false)
}

/// The nearest ancestor holding a `Cargo.toml` beside a `src/` directory.
fn project_root() -> Result<PathBuf, String> {
    let mut current = std::env::current_dir().map_err(|err| err.to_string())?;

    loop {
        if current.join("Cargo.toml").is_file() && current.join("src").is_dir() {
            return Ok(current);
        }

        if !current.pop() {
            return Err(
                "run this inside a Luxid application (no Cargo.toml with a src/ directory found)"
                    .to_owned(),
            );
        }
    }
}

/// `YYYYMMDD_HHMMSS`, so migrations sort chronologically.
fn timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();

    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    let rest = seconds % 86_400;

    format!(
        "{year:04}{month:02}{day:02}_{:02}{:02}{:02}",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// Howard's `civil_from_days`. Formatting one timestamp does not justify a
/// date-and-time dependency in a scaffolding binary.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;

    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn report(subject: &str, written: &[PathBuf]) {
    println!("Created {subject}:");

    for path in written {
        println!("    {}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_a_known_instant() {
        // Day 20687 of the Unix epoch is 2026-08-22.
        assert_eq!(civil_from_days(20_687), (2026, 8, 22));
        assert_eq!(civil_from_days(20_688), (2026, 8, 23));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }

    #[test]
    fn a_timestamp_is_sortable_and_the_right_shape() {
        let stamp = timestamp();

        assert_eq!(stamp.len(), 15, "YYYYMMDD_HHMMSS");
        assert_eq!(stamp.chars().nth(8), Some('_'));
        assert!(stamp.starts_with("20"));
    }
}
