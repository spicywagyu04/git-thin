use clap::{Parser, Subcommand};
use git_thin::{git::Repo, lifecycle};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "git thin",
    bin_name = "git thin",
    version,
    about = "Independent copy-on-write Git worktrees with editing-scope validation"
)]
struct Cli {
    /// Emit machine-readable JSON (errors included).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Check the source checkout and native cloning support.
    Doctor,
    /// Create a new branch and a worktree at the source HEAD.
    Add {
        destination: PathBuf,
        #[arg(long)]
        branch: String,
        /// Exact file or recursive directory, relative to the source root.
        #[arg(long = "write")]
        write: Vec<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Expand the intended editing scope (writes are not blocked).
    Scope {
        #[command(subcommand)]
        command: ScopeCommands,
    },
    /// List managed worktrees and unfinished operations in this repository.
    List,
    /// Inspect metadata and scope compliance.
    Status { worktree: PathBuf },
    /// Validate current committed, staged, and unstaged changes against scope.
    Check { worktree: PathBuf },
    /// Remove a clean managed worktree, preserving its branch and commits.
    Remove {
        worktree: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    /// Recover interrupted operations using recorded ownership.
    Recover {
        #[arg(long)]
        dry_run: bool,
    },
}
#[derive(Subcommand)]
enum ScopeCommands {
    Add {
        worktree: PathBuf,
        #[arg(required = true)]
        paths: Vec<String>,
    },
}

fn execute(cli: &Cli) -> Result<(Value, i32), String> {
    let repo = Repo::discover(&std::env::current_dir().map_err(|e| e.to_string())?)?;
    let value = match &cli.command {
        Commands::Doctor => lifecycle::doctor(&repo)?,
        Commands::Add {
            destination,
            branch,
            write,
            dry_run,
        } => lifecycle::add(&repo, destination, branch, write, *dry_run)?,
        Commands::Scope {
            command: ScopeCommands::Add { worktree, paths },
        } => lifecycle::expand_scope(&repo, worktree, paths)?,
        Commands::List => lifecycle::list(&repo)?,
        Commands::Status { worktree } => lifecycle::status(&repo, worktree)?,
        Commands::Check { worktree } => {
            let result = lifecycle::check(&repo, worktree)?;
            let code = if result.clean { 0 } else { 1 };
            return Ok((
                serde_json::to_value(result).map_err(|e| e.to_string())?,
                code,
            ));
        }
        Commands::Remove { worktree, dry_run } => lifecycle::remove(&repo, worktree, *dry_run)?,
        Commands::Recover { dry_run } => lifecycle::recover(&repo, *dry_run)?,
    };
    Ok((value, 0))
}

fn human(value: &Value) {
    if let Some(violations) = value.get("violations").and_then(Value::as_array) {
        if violations.is_empty() {
            println!(
                "Editing scope check passed (final-state validation; writes are not blocked)."
            );
        } else {
            println!("Changes outside the declared editing scope:");
            for path in violations {
                println!("  {path}");
            }
            println!(
                "All changes have been preserved. Expand the scope if these edits are intended."
            );
        }
    } else {
        // JSON-style quoting keeps newlines and control characters in paths unambiguous.
        println!(
            "{}",
            serde_json::to_string_pretty(value).expect("serializable output")
        );
    }
}
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    let wants_json = args.iter().any(|a| a == "--json");
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            if wants_json && error.use_stderr() {
                println!("{}", json!({"error":error.to_string(),"exit_code":2}));
                std::process::exit(2);
            }
            error.exit();
        }
    };
    match execute(&cli) {
        Ok((value, code)) => {
            if cli.json {
                println!(
                    "{}",
                    serde_json::to_string(&value).expect("serializable output")
                );
            } else {
                human(&value);
            }
            std::process::exit(code);
        }
        Err(error) => {
            if cli.json {
                println!("{}", json!({"error":error,"exit_code":2}));
            } else {
                eprintln!("git thin: {error}");
            }
            std::process::exit(2);
        }
    }
}
