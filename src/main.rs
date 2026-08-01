use anyhow::Context;
use clap::Parser;
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
};
use tokio::fs;
use tracing::{Level, info};

#[derive(Parser, Debug)]
#[command()]
struct Args {
    #[arg(long)]
    ticks_max: u32,

    #[arg(long)]
    codesize_max: u32,

    /// path to risc-v as binary
    #[arg(long)]
    as_binary: PathBuf,

    /// path to risc-v ld binary
    #[arg(long)]
    ld_binary: PathBuf,

    /// path to risc-v simulator binary
    #[arg(long)]
    simulator_binary: PathBuf,

    /// path to a folder, where submissions will be stored
    #[arg(long, default_value = "submission")]
    submissions_folder: PathBuf,

    /// github client id
    #[arg(long)]
    client_id: String,

    /// name of database
    #[arg(long, default_value_t = String::from("riscv_sim"))]
    db_name: String,

    /// path to file with jwt token
    #[arg(long)]
    jwt_token_path: PathBuf,
}

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

    let args = Args::parse();

    let mongo_uri =
        std::env::var("MONGODB_URI").unwrap_or_else(|_| "mongodb://localhost:27017".to_string());

    let client_secret =
        std::env::var("GITHUB_CLIENT_SECRET").context("GITHUB_CLIENT_SECRET not set")?;

    let jwt_secret_path = args.jwt_token_path;
    let jwt_secret = fs::read_to_string(&jwt_secret_path)
        .await
        .with_context(|| format!("failed to read JWT secret from {jwt_secret_path:?}"))?;

    risc_v_sim_web::run(
        tracing::info_span!("rvsim-web"),
        listener,
        risc_v_sim_web::Config {
            as_binary: args.as_binary,
            ld_binary: args.ld_binary,
            simulator_binary: args.simulator_binary,
            submissions_folder: args.submissions_folder,
            ticks_max: args.ticks_max,
            codesize_max: args.codesize_max,
            mongo_uri,
            db_name: args.db_name,
            client_id: args.client_id,
            client_secret,
            jwt_secret,
            auth_url: "https://github.com/login/oauth/authorize".to_string(),
            token_url: "https://github.com/login/oauth/access_token".to_string(),
        },
    )
    .await
}
