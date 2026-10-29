use clap::Parser;
use hmesh_mcp::{MAX_FRAME_BYTES, OwnerBackend, Protocol};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[derive(Parser)]
struct Args {
    #[arg(long)]
    data_dir: std::path::PathBuf,
    #[arg(long)]
    parent_task: uuid::Uuid,
}
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("hmesh-mcp stopped: invalid framing or unavailable owner-scoped context");
            std::process::ExitCode::from(1)
        }
    }
}
async fn run() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut protocol = Protocol::new(OwnerBackend::load(&args.data_dir, args.parent_task)?);
    let mut input = tokio::io::stdin();
    let mut output = tokio::io::stdout();
    let mut chunk = [0u8; 8192];
    let mut pending = vec![];
    loop {
        let count = input.read(&mut chunk).await?;
        if count == 0 {
            anyhow::ensure!(pending.is_empty(), "truncated MCP frame");
            break;
        }
        for byte in &chunk[..count] {
            if *byte == b'\n' {
                if pending.last() == Some(&b'\r') {
                    pending.pop();
                }
                let response = match serde_json::from_slice(&pending) {
                    Ok(request) => protocol.handle(request).await,
                    Err(_) => Some(
                        json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
                    ),
                };
                pending.clear();
                if let Some(response) = response {
                    let bytes = serde_json::to_vec(&response)?;
                    anyhow::ensure!(bytes.len() <= MAX_FRAME_BYTES, "MCP result exceeded bound");
                    output.write_all(&bytes).await?;
                    output.write_all(b"\n").await?;
                    output.flush().await?;
                }
            } else {
                anyhow::ensure!(
                    pending.len() < MAX_FRAME_BYTES,
                    "MCP request exceeded bound"
                );
                pending.push(*byte);
            }
        }
    }
    Ok(())
}
