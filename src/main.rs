use anyhow::Context;
use clap::Parser;
use std::net::{Ipv4Addr, SocketAddrV4};
use tracing::{Level, info};

#[derive(Parser, Debug)]
#[command()]
struct Args {
    #[arg(long)]
    ticks_max: u32,

    #[arg(long)]
    codesize_max: u32,

    /// path to risc-v as binary
    #[arg(long, default_value_t = String::from("riscv64-elf-as"))]
    as_binary: String,

    /// path to risc-v ld binary
    #[arg(long, default_value_t = String::from("riscv64-elf-ld"))]
    ld_binary: String,

    /// path to risc-v simulator binary
    #[arg(long, default_value_t = String::from("simulator"))]
    simulator_binary: String,

    /// path to a folder, where submissions will be stored
    #[arg(long, default_value_t = String::from("submission"))]
    submissions_folder: String,

    /// github client id
    #[arg(long)]
    client_id: String,

    /// name of database
    #[arg(long, default_value_t = String::from("riscv_sim"))]
    db_name: String,
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
    let jwt_secret = std::env::var("JWT_SECRET").context("JWT_SECRET not set")?;

    risc_v_sim_web::run(
        tracing::info_span!("rvsim-web"),
        listener,
        risc_v_sim_web::Config {
            as_binary: args.as_binary.into(),
            ld_binary: args.ld_binary.into(),
            simulator_binary: args.simulator_binary.into(),
            submissions_folder: args.submissions_folder.into(),
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
