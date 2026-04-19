use anyhow::Context;
use std::net::{Ipv4Addr, SocketAddrV4};
use tracing::{Level, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_level(true)
        .with_max_level(Level::INFO)
        .init();

    let address = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 3000);
    let listener = tokio::net::TcpListener::bind(address).await?;
    let port = listener.local_addr()?.port();
    info!(port = port, "Starting...");

    let ticks_max: u32 = std::env::var("TICKS_MAX")?.parse()?;
    let codesize_max: u32 = std::env::var("CODESIZE_MAX")?.parse()?;
    let as_binary = std::env::var("AS_BINARY")
        .unwrap_or_else(|_| "riscv64-elf-as".to_string())
        .into();
    let ld_binary = std::env::var("LD_BINARY")
        .unwrap_or_else(|_| "riscv64-elf-ld".to_string())
        .into();
    let simulator_binary = std::env::var("SIMULATOR_BINARY")
        .unwrap_or_else(|_| "simulator".to_string())
        .into();
    let submissions_folder = std::env::var("SUBMISSIONS_FOLDER")
        .unwrap_or_else(|_| "submission".to_string())
        .into();

    let mongo_uri =
        std::env::var("MONGODB_URI").unwrap_or_else(|_| "mongodb://localhost:27017".to_string());
    let db_name = std::env::var("MONGODB_DB").unwrap_or_else(|_| "riscv_sim".to_string());

    let client_id = std::env::var("GITHUB_CLIENT_ID").context("GITHUB_CLIENT_ID not set")?;
    let client_secret =
        std::env::var("GITHUB_CLIENT_SECRET").context("GITHUB_CLIENT_SECRET not set")?;
    let jwt_secret = std::env::var("JWT_SECRET").context("JWT_SECRET not set")?;

    risc_v_sim_web::run(
        tracing::info_span!("rvsim-web"),
        listener,
        risc_v_sim_web::Config {
            as_binary,
            ld_binary,
            simulator_binary,
            submissions_folder,
            ticks_max,
            codesize_max,
            mongo_uri,
            db_name,
            client_id,
            client_secret,
            jwt_secret,
            auth_url: "https://github.com/login/oauth/authorize".to_string(),
            token_url: "https://github.com/login/oauth/access_token".to_string(),
        },
    )
    .await
}
