use sauerteig_server::{serve, Config};
use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Sauerteig server: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let mut config = Config::default();
    if let Ok(path) = std::env::var("SAUERTEIG_WORLD") {
        config.world_path = path.into();
    }
    let mut bind = std::env::var("SAUERTEIG_BIND").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bind" => bind = args.next().ok_or("--bind needs an IP:port")?,
            "--world" => config.world_path = args.next().ok_or("--world needs a path")?.into(),
            "--no-edit" => config.editing_allowed = false,
            "--help" | "-h" => {
                println!("sauerteig-server [--bind 127.0.0.1:3000] [--world .local/world.json] [--no-edit]\n\nFor trusted LAN play, use --bind 0.0.0.0:3000. This prototype has no account authentication.");
                return Ok(());
            }
            _ => return Err(format!("Unknown option: {arg}")),
        }
    }
    let address: SocketAddr = bind
        .parse()
        .map_err(|e| format!("Invalid bind address: {e}"))?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "Sauerteig listening at ws://{}/ws",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    serve(listener, config, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}
