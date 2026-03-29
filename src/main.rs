//! CLI entry point for code-review-graph.
//!
//! Usage:
//!   code-review-graph build [--repo PATH]
//!   code-review-graph update [--base REF] [--repo PATH]
//!   code-review-graph status [--repo PATH]
//!   code-review-graph watch [--repo PATH]
//!   code-review-graph visualize [--repo PATH]
//!   code-review-graph serve [--repo PATH]
//!   code-review-graph install [--repo PATH] [--dry-run]
//!   code-review-graph config set <key> <value>
//!   code-review-graph config get <key>
//!   code-review-graph config list
//!   code-review-graph config reset

use camino::Utf8PathBuf;
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
        #[arg(short, long)]
        quiet: bool,
    },
    /// Incremental update (only changed files)
    Update {
        #[arg(long, default_value = "HEAD~1")]
        base: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(short, long)]
        quiet: bool,
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
        /// Which tools to expose in tools/list: "core" (3 tools, default) or "all" (13 tools)
        #[arg(long, default_value = "core")]
        tools: String,
    },
    /// Register MCP server with Claude Code (creates .mcp.json)
    Install {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Manage configuration (API keys, embedding provider)
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Set a configuration value
    Set {
        /// Config key (e.g., embedding-provider, openai-api-key)
        key: String,
        /// Value to set
        value: String,
    },
    /// Get a configuration value
    Get {
        /// Config key
        key: String,
    },
    /// List all configuration values (API keys are masked)
    List,
    /// Reset (delete) the configuration file
    Reset,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();
    let cli = Cli::parse();

    match cli.command {
        None => {
            print_banner();
            Ok(())
        }
        Some(Commands::Serve { repo, tools }) => {
            code_review_graph::server::run_server(repo, &tools).await?;
            Ok(())
        }
        Some(cmd) => {
            handle_command(cmd).await?;
            Ok(())
        }
    }
}

async fn handle_command(cmd: Commands) -> anyhow::Result<()> {
    use code_review_graph::incremental;

    match cmd {
        Commands::Build { repo, quiet } => {
            let root = resolve_project_root(repo.as_deref(), false)?;
            let db_path = incremental::get_db_path(&root);
            let mut store = code_review_graph::graph::GraphStore::new(&db_path)?;
            let result = incremental::full_build(&root, &mut store)?;
            if !quiet {
                println!(
                    "Full build: {} files, {} nodes, {} edges",
                    result.files_parsed, result.total_nodes, result.total_edges
                );
            }
            if !result.errors.is_empty() {
                eprintln!("Errors: {}", result.errors.len());
            }
            store.close()?;
        }

        Commands::Update { base, repo, quiet } => {
            let root = resolve_project_root(repo.as_deref(), true)?;
            let db_path = incremental::get_db_path(&root);
            let mut store = code_review_graph::graph::GraphStore::new(&db_path)?;
            let result = incremental::incremental_update(&root, &mut store, &base, None)?;
            if !quiet {
                println!(
                    "Incremental: {} files updated, {} nodes, {} edges",
                    result.files_updated, result.total_nodes, result.total_edges
                );
            }
            store.close()?;
        }

        Commands::Status { repo } => {
            let root = resolve_project_root(repo.as_deref(), false)?;
            let db_path = incremental::get_db_path(&root);
            let store = code_review_graph::graph::GraphStore::new(&db_path)?;
            let stats = store.get_stats()?;
            println!("Nodes: {}", stats.total_nodes);
            println!("Edges: {}", stats.total_edges);
            println!("Files: {}", stats.files_count);
            println!("Languages: {}", stats.languages.join(", "));
            println!(
                "Last updated: {}",
                stats.last_updated.as_deref().unwrap_or("never")
            );
            store.close()?;
        }

        Commands::Watch { repo } => {
            let root = resolve_project_root(repo.as_deref(), false)?;
            let db_path = incremental::get_db_path(&root);
            let mut store = code_review_graph::graph::GraphStore::new(&db_path)?;
            incremental::watch(&root, &mut store)?;
            store.close()?;
        }

        Commands::Visualize { repo } => {
            let root = resolve_project_root(repo.as_deref(), false)?;
            let db_path = incremental::get_db_path(&root);
            let store = code_review_graph::graph::GraphStore::new(&db_path)?;
            let html_path = root.join(".code-review-graph").join("graph.html");
            code_review_graph::visualization::generate_html(&store, &html_path)?;
            println!("Visualization: {}", html_path);
            println!("Open in browser to explore your codebase graph.");
            store.close()?;
        }

        Commands::Install { repo, dry_run } => {
            handle_install(repo.as_deref(), dry_run)?;
        }

        Commands::Config { action } => {
            handle_config(action)?;
        }

        // Serve is handled in main() before reaching here.
        Commands::Serve { .. } => unreachable!(),
    }

    Ok(())
}

/// Resolve the repository/project root.
///
/// `require_git`: if true (for `update`), the root must be inside a git repo.
fn resolve_project_root(
    repo: Option<&str>,
    require_git: bool,
) -> anyhow::Result<Utf8PathBuf> {
    use code_review_graph::incremental;

    if let Some(r) = repo {
        let path = Utf8PathBuf::from(r);
        if !path.is_dir() {
            anyhow::bail!("Repository path is not a directory: {}", path);
        }
        return Ok(path);
    }

    if require_git {
        match incremental::find_repo_root(None) {
            Some(root) => Ok(root),
            None => anyhow::bail!(
                "Not in a git repository. 'update' requires git for diffing.\n\
                 Use 'build' for a full parse, or run 'git init' first."
            ),
        }
    } else {
        Ok(incremental::find_project_root(None))
    }
}

/// Create or merge `.mcp.json` in the project root.
fn handle_install(repo: Option<&str>, dry_run: bool) -> anyhow::Result<()> {
    use code_review_graph::incremental;

    let root: Utf8PathBuf = if let Some(r) = repo {
        Utf8PathBuf::from(r)
    } else {
        incremental::find_project_root(None)
    };

    let mcp_path = root.join(".mcp.json");

    let entry = serde_json::json!({
        "mcpServers": {
            "code-review-graph": {
                "command": "code-review-graph",
                "args": ["serve"]
            }
        }
    });

    let final_config: serde_json::Value = if mcp_path.exists() {
        let content = std::fs::read_to_string(&mcp_path)?;
        match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(mut existing) => {
                // Already configured?
                if existing
                    .get("mcpServers")
                    .and_then(|s| s.get("code-review-graph"))
                    .is_some()
                {
                    println!("Already configured in {}", mcp_path);
                    return Ok(());
                }
                // Merge our entry in
                if let Some(servers) = existing
                    .get_mut("mcpServers")
                    .and_then(|v| v.as_object_mut())
                {
                    servers.insert(
                        "code-review-graph".to_string(),
                        entry["mcpServers"]["code-review-graph"].clone(),
                    );
                } else {
                    existing["mcpServers"] = entry["mcpServers"].clone();
                }
                existing
            }
            Err(_) => {
                eprintln!(
                    "Warning: existing {} has invalid JSON, overwriting.",
                    mcp_path
                );
                entry
            }
        }
    } else {
        entry
    };

    let json_str = serde_json::to_string_pretty(&final_config)?;

    if dry_run {
        println!("[dry-run] Would write to {}:", mcp_path);
        println!("{}", json_str);
        println!();
        println!("[dry-run] No files were modified.");
        return Ok(());
    }

    std::fs::write(&mcp_path, [json_str.as_bytes(), b"\n"].concat())?;
    println!("Created {}", mcp_path);
    println!();
    println!("Next steps:");
    println!("  1. code-review-graph build    # build the knowledge graph");
    println!("  2. Restart Claude Code        # to pick up the new MCP server");

    Ok(())
}

fn handle_config(action: ConfigAction) -> anyhow::Result<()> {
    use code_review_graph::config::{AppConfig, display_value, validate_config_key};

    match action {
        ConfigAction::Set { key, value } => {
            validate_config_key(&key)?;
            let mut config = AppConfig::load();
            let display = display_value(&key, &value);
            config.set(&key, &value);
            config.save()?;
            println!("Set {} = {}", key, display);
            println!("Config: {}", AppConfig::config_path().display());
        }
        ConfigAction::Get { key } => {
            let config = AppConfig::load();
            match config.get(&key) {
                Some(v) => println!("{}: {}", key, display_value(&key, v)),
                None => println!("{}: (not set)", key),
            }
        }
        ConfigAction::List => {
            let config = AppConfig::load();
            if config.values.is_empty() {
                println!("No configuration set.");
                println!("Run: code-review-graph config set embedding-provider openai");
                return Ok(());
            }
            for (k, v) in &config.values {
                println!("  {}: {}", k, display_value(k, v));
            }
        }
        ConfigAction::Reset => {
            let path = AppConfig::config_path();
            match std::fs::remove_file(&path) {
                Ok(()) => println!("Configuration reset."),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    println!("No configuration file found.");
                }
                Err(e) => anyhow::bail!("Failed to reset config: {e}"),
            }
        }
    }
    Ok(())
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
    config      Manage API keys and preferences

  Run code-review-graph <command> --help for details
"#,
        version
    );
}
