mod access;
pub mod admin;
mod auth;
mod browse;
mod device;
mod error;
mod http;
mod retrieval;
mod snapshot;

use std::time::Duration;

use anyhow::Context;
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

pub use access::Access;
pub use http::{Reader, router};

pub async fn connect(database_url: &str) -> anyhow::Result<PgPool> {
    connect_pool(database_url, false).await
}

/// A small pool whose transactions default to read-only, for archive reads.
pub async fn connect_read(database_url: &str) -> anyhow::Result<PgPool> {
    connect_pool(database_url, true).await
}

async fn connect_pool(database_url: &str, read_only: bool) -> anyhow::Result<PgPool> {
    let mut options = database_url.parse::<PgConnectOptions>()?.options([
        ("statement_timeout", "10s"),
        ("lock_timeout", "5s"),
        ("idle_in_transaction_session_timeout", "15s"),
    ]);
    if read_only {
        options = options.options([("default_transaction_read_only", "on")]);
    }
    PgPoolOptions::new()
        .max_connections(if read_only { 2 } else { 5 })
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await
        .context("could not connect to PostgreSQL")
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!().run(pool).await.context("migration failed")
}
