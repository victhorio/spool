use anyhow::{Context, bail};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{auth, snapshot::valid_id};

#[derive(Serialize)]
pub struct Credential {
    pub source_id: Uuid,
    pub token: String,
}

pub async fn add_source(pool: &PgPool, label: &str) -> anyhow::Result<Credential> {
    if label.trim().is_empty() || label.len() > 256 || label.contains('\0') {
        bail!("label must be nonempty, at most 256 bytes, and contain no NUL");
    }
    let source_id = Uuid::new_v4();
    let (token, hash) = auth::issue(source_id);
    sqlx::query("INSERT INTO sources (id, label, token_hash) VALUES ($1, $2, $3)")
        .bind(source_id)
        .bind(label)
        .bind(hash)
        .execute(pool)
        .await?;
    Ok(Credential { source_id, token })
}

pub async fn rotate_source(pool: &PgPool, source_id: Uuid) -> anyhow::Result<Credential> {
    let (token, hash) = auth::issue(source_id);
    let updated =
        sqlx::query("UPDATE sources SET token_hash = $2 WHERE id = $1 AND revoked_at IS NULL")
            .bind(source_id)
            .bind(hash)
            .execute(pool)
            .await?
            .rows_affected();
    if updated == 0 {
        bail!("source not found or revoked");
    }
    Ok(Credential { source_id, token })
}

pub async fn revoke_source(pool: &PgPool, source_id: Uuid) -> anyhow::Result<()> {
    let updated =
        sqlx::query("UPDATE sources SET revoked_at = COALESCE(revoked_at, now()) WHERE id = $1")
            .bind(source_id)
            .execute(pool)
            .await?
            .rows_affected();
    if updated == 0 {
        bail!("source not found");
    }
    Ok(())
}

pub async fn purge_session(pool: &PgPool, source: Uuid, session: &str) -> anyhow::Result<i64> {
    if !valid_id(session) {
        bail!("invalid session ID");
    }
    let mut tx = pool.begin().await?;
    let created = sqlx::query(
        "INSERT INTO sessions (source_id, session_id, revision, format, deleted_at) \
         VALUES ($1, $2, 1, 'opencode.v2', now()) ON CONFLICT (source_id, session_id) DO NOTHING",
    )
    .bind(source)
    .bind(session)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    let current: i64 = sqlx::query_scalar(
        "SELECT revision FROM sessions WHERE source_id = $1 AND session_id = $2 FOR UPDATE",
    )
    .bind(source)
    .bind(session)
    .fetch_one(&mut *tx)
    .await?;
    let revision = if created {
        1
    } else {
        current.checked_add(1).context("revision exhausted")?
    };
    sqlx::query("DELETE FROM messages WHERE source_id = $1 AND session_id = $2")
        .bind(source)
        .bind(session)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE sessions SET revision = $3, info = NULL, info_hash = NULL, \
         deleted_at = COALESCE(deleted_at, now()), received_at = now() \
         WHERE source_id = $1 AND session_id = $2",
    )
    .bind(source)
    .bind(session)
    .bind(revision)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(revision)
}
