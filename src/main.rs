use clap::Parser;
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
};
use tracing::{Level, info};

#[derive(Parser, Debug)]
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

    /// name of database
    #[arg(long, default_value_t = String::from("riscv_sim"))]
    db_name: String,

    #[cfg(feature = "jwt_authorization")]
    #[command(flatten)]
    jwt_authorization: risc_v_sim_web::auth::jwt_authorization::AuthArgs,

    #[cfg(feature = "github_authentication")]
    #[command(flatten)]
    github_authentication: risc_v_sim_web::auth::github_authentication::AuthArgs,
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

            #[cfg(feature = "jwt_authorization")]
            jwt_authorization: risc_v_sim_web::auth::jwt_authorization::AuthConfig::from_flags(
                args.jwt_authorization,
            ),

            #[cfg(feature = "github_authentication")]
            github_authentication:
                risc_v_sim_web::auth::github_authentication::AuthConfig::from_flags(
                    args.github_authentication,
                )?,
        },
    )
    .await
}
