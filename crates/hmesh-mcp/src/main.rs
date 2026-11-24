use clap::Parser;

/// HarnessMesh MCP server over stdio. Add it to any agent's MCP configuration.
#[derive(Parser)]
struct Args {
    /// Node directory; defaults to ~/.harnessmesh like the `hmesh` CLI.
    #[arg(long)]
    data_dir: Option<std::path::PathBuf>,
    /// Set by the runtime for a managed Pi parent run: confines the tools to
    /// that run's owner scope file, budget and own child tasks.
    #[arg(long, hide = true)]
    parent_task: Option<uuid::Uuid>,
}
#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args = Args::parse();
    let dir = args.data_dir.unwrap_or_else(hmesh_mcp::default_data_dir);
    match hmesh_mcp::serve_stdio(&dir, args.parent_task).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hmesh-mcp stopped: {error}");
            std::process::ExitCode::from(1)
        }
    }
}
