use winvision_mcp::{actor::Actor, init_dpi, mcp};

fn main() {
    init_dpi();
    let actor = Actor::spawn();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    eprintln!(
        "winvision-mcp v{}: ready on stdio (MCP, newline-delimited JSON-RPC 2.0)",
        env!("CARGO_PKG_VERSION")
    );
    if let Err(e) = rt.block_on(mcp::run(actor)) {
        eprintln!("winvision-mcp: stdio loop failed: {e}");
        std::process::exit(1);
    }
    eprintln!("winvision-mcp: stdin closed, exiting");
}
