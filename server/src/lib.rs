pub mod admin;
mod auth;
mod browse;
mod error;
mod http;
mod snapshot;

use std::time::Duration;

use anyhow::Context;
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

pub use browse::{browse_router, validate_browse_bind};
pub use http::router;

pub async fn connect(database_url: &str) -> anyhow::Result<PgPool> {
    connect_pool(database_url, false).await
}

pub async fn connect_browse(database_url: &str) -> anyhow::Result<PgPool> {
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
