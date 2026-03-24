//! CLI entry point for code-review-graph.
//!
//! Usage:
//!   code-review-graph build [--repo PATH]
//!   code-review-graph update [--base REF] [--repo PATH]
//!   code-review-graph status [--repo PATH]
//!   code-review-graph watch [--repo PATH]
//!   code-review-graph visualize [--repo PATH]
//!   code-review-graph serve [--repo PATH]
//!   code-review-graph install [--repo PATH]

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "code-review-graph",
    about = "Persistent incremental knowledge graph for code reviews",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Full graph build (re-parse all files)
    Build {
        #[arg(long)]
        repo: Option<String>,
    },
    /// Incremental update (only changed files)
    Update {
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        #[arg(long)]
        repo: Option<String>,
    },
    /// Show graph statistics
    Status {
        #[arg(long)]
        repo: Option<String>,
    },
    /// Watch for changes and auto-update
    Watch {
        #[arg(long)]
        repo: Option<String>,
    },
    /// Generate interactive HTML graph visualization
    Visualize {
        #[arg(long)]
        repo: Option<String>,
    },
    /// Start MCP server (stdio transport)
    Serve {
        #[arg(long)]
        repo: Option<String>,
    },
    /// Register MCP server with Claude Code (creates .mcp.json)
    Install {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    match cli.command {
        None => {
            print_banner();
            Ok(())
        }
        Some(Commands::Serve { repo }) => {
            code_review_graph::server::run_server(repo).await?;
            Ok(())
        }
        Some(cmd) => {
            handle_command(cmd).await?;
            Ok(())
        }
    }
}

async fn handle_command(cmd: Commands) -> anyhow::Result<()> {
    let _ = cmd;
    todo!("Implement CLI command dispatch")
}

fn print_banner() {
    let version = env!("CARGO_PKG_VERSION");
    println!(
        r#"
  *--*--*
  |\ | /|       code-review-graph  v{}
  *--@--*
  |/ | \|       Structural knowledge graph for
  *--*--*       smarter code reviews

  Commands:
    install     Set up Claude Code integration
    build       Full graph build (parse all files)
    update      Incremental update (changed files only)
    watch       Auto-update on file changes
    status      Show graph statistics
    visualize   Generate interactive HTML graph
    serve       Start MCP server
"#,
        version
    );
}
