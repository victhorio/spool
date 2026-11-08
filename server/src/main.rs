use std::{env, net::IpAddr};

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde_json::json;
use spool_server::{admin, connect, migrate, router};
use uuid::Uuid;

#[derive(Parser)]
#[command(version, about = "Spool session archive ingestion service")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve HTTP; applies pending migrations before listening (the default).
    Serve,
    /// Apply pending database migrations and exit.
    Migrate,
    /// Manage source credentials using direct database access.
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    /// Permanently remove archived session bodies using direct database access.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
}

#[derive(Subcommand)]
enum SourceCommand {
    Add {
        #[arg(long)]
        label: String,
    },
    Rotate {
        source_id: Uuid,
    },
    Revoke {
        source_id: Uuid,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    Purge { source_id: Uuid, session_id: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let database_url = env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let pool = connect(&database_url).await?;
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            migrate(&pool).await?;
            let bind: IpAddr = env::var("BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0".into())
                .parse()
                .context("BIND_ADDR must be an IP address")?;
            let port: u16 = env::var("PORT")
                .unwrap_or_else(|_| "8080".into())
                .parse()
                .context("PORT must be a valid port")?;
            let listener = tokio::net::TcpListener::bind((bind, port)).await?;
            eprintln!("Spool listening on {}", listener.local_addr()?);
            axum::serve(listener, router(pool.clone()))
                .with_graceful_shutdown(shutdown())
                .await?;
        }
        Command::Migrate => migrate(&pool).await?,
        Command::Source { command } => match command {
            SourceCommand::Add { label } => {
                println!(
                    "{}",
                    serde_json::to_string(&admin::add_source(&pool, &label).await?)?
                );
            }
            SourceCommand::Rotate { source_id } => {
                println!(
                    "{}",
                    serde_json::to_string(&admin::rotate_source(&pool, source_id).await?)?
                );
            }
            SourceCommand::Revoke { source_id } => {
                admin::revoke_source(&pool, source_id).await?;
                println!("{}", json!({"source_id": source_id, "revoked": true}));
            }
        },
        Command::Session { command } => match command {
            SessionCommand::Purge {
                source_id,
                session_id,
            } => {
                let revision = admin::purge_session(&pool, source_id, &session_id).await?;
                println!(
                    "{}",
                    json!({"source_id": source_id, "session_id": session_id, "revision": revision.to_string(), "deleted": true})
                );
            }
        },
    }
    pool.close().await;
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = terminate.recv() => {},
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
