use std::{env, net::IpAddr};

use anyhow::Context;
use clap::{Parser, Subcommand};
use serde_json::json;
use spool_server::{Access, Reader, admin, connect, connect_read, migrate, router};
use uuid::Uuid;

#[derive(Parser)]
#[command(version, about = "Spool session archive service")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve HTTP; applies pending migrations before listening (the default).
    /// ACCESS_ISSUER and ACCESS_AUD enable the web reader behind Cloudflare Access.
    Serve,
    /// Apply pending database migrations and exit.
    Migrate,
    /// Manage source credentials using direct database access.
    Source {
        #[command(subcommand)]
        command: SourceCommand,
    },
    /// Permanently remove archived session bodies, and the artifacts the session
    /// created, using direct database access.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Permanently remove artifacts using direct database access.
    Artifact {
        #[command(subcommand)]
        command: ArtifactCommand,
    },
}

#[derive(Subcommand)]
enum ArtifactCommand {
    Purge { id: String },
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
    let command = cli.command.unwrap_or(Command::Serve);
    let reader = match command {
        Command::Serve => reader()?,
        _ => None,
    };
    let database_url = env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let pool = connect(&database_url).await?;
    match command {
        Command::Serve => {
            let enabled = reader.is_some();
            let read = connect_read(&database_url).await?;
            let app = router(pool.clone(), read.clone(), reader)?;
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
            eprintln!(
                "Spool listening on {} (web reader {})",
                listener.local_addr()?,
                if enabled { "enabled" } else { "disabled" }
            );
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown())
                .await?;
            read.close().await;
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
        Command::Artifact {
            command: ArtifactCommand::Purge { id },
        } => {
            admin::purge_artifact(&pool, &id).await?;
            println!("{}", json!({"id": id, "deleted": true}));
        }
    }
    pool.close().await;
    Ok(())
}

fn reader() -> anyhow::Result<Option<Reader>> {
    let access = match (env::var("ACCESS_ISSUER"), env::var("ACCESS_AUD")) {
        (Ok(issuer), Ok(audience)) => Access::new(&issuer, &audience)?,
        (Err(_), Err(_)) => return Ok(None),
        _ => anyhow::bail!("set both ACCESS_ISSUER and ACCESS_AUD, or neither"),
    };
    Ok(Some(Reader {
        access,
        assets: env::var("ASSETS_DIR")
            .unwrap_or_else(|_| "web/dist".into())
            .into(),
    }))
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
