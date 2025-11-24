use anyhow::Result;
use std::net::{Ipv4Addr, SocketAddrV4};
use tracing::{Level, info};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_level(true)
        .with_max_level(Level::INFO)
        .init();

    let address = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 3000);
    let listener = tokio::net::TcpListener::bind(address).await?;
    let port = listener.local_addr()?.port();
    info!("Listening on port {port}");

    let ticks_max: u32 = std::env::var("TICKS_MAX")?.parse()?;
    let codesize_max: u32 = std::env::var("CODESIZE_MAX")?.parse()?;
    let auth_state = risc_v_sim_web::auth::create_auth_state()?;

    // Initialize database
    risc_v_sim_web::database::init_database().await?;

    risc_v_sim_web::run(
        tracing::info_span!("rvsim-web"),
        listener,
        risc_v_sim_web::Config {
            as_binary: std::env::var("AS_BINARY")
                .unwrap_or_else(|_| "riscv64-elf-as".to_string())
                .into(),
            ld_binary: std::env::var("LD_BINARY")
                .unwrap_or_else(|_| "riscv64-elf-ld".to_string())
                .into(),
            simulator_binary: std::env::var("SIMULATOR_BINARY")
                .unwrap_or_else(|_| "simulator".to_string())
                .into(),
            submissions_folder: std::env::var("SUBMISSIONS_FOLDER")
                .unwrap_or_else(|_| "submission".to_string())
                .into(),
            ticks_max: ticks_max,
            codesize_max: codesize_max,
            auth_state,
        },
    )
    .await;

    Ok(())
}
