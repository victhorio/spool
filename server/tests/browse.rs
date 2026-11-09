use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::DateTime;
use reqwest::{Client, Response, StatusCode};
use serde_json::{Value, json};
use spool_server::{browse_router, connect, connect_browse, migrate, router, validate_browse_bind};
use sqlx::{Executor, PgPool, postgres::PgPoolOptions};
use tokio::task::JoinHandle;
use uuid::Uuid;

const A: Uuid = Uuid::from_u128(1);
const B: Uuid = Uuid::from_u128(2);
const RAW_INFO: &str = r#"{ "id":"same", "unknown":{"text":"x\u0000y","number":1e3}, "html":"<script>alert(1)</script>" }"#;
const RAW_MESSAGE: &str =
    r#"{ "id":"m3", "parts":[{"type":"future","text":"a\u0000b"}], "number":1e3 }"#;

struct Assets(PathBuf);

impl Assets {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("spool-browse-{}", Uuid::new_v4()));
        std::fs::create_dir_all(path.join("assets")).unwrap();
        std::fs::write(path.join("index.html"), "<!doctype html><title>Browse test</title><script type=module src=/assets/test.js></script>").unwrap();
        std::fs::write(path.join("assets/test.js"), "document.title = 'Synthetic';").unwrap();
        std::fs::write(path.join("private.txt"), "must not be served").unwrap();
        Self(path)
    }
}

impl Drop for Assets {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

async fn serve(app: axum::Router) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, task)
}

fn privacy(response: &Response) {
    let headers = response.headers();
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["x-frame-options"], "DENY");
    assert_eq!(headers["cross-origin-resource-policy"], "same-origin");
    assert!(headers.get("access-control-allow-origin").is_none());
    let csp = headers["content-security-policy"].to_str().unwrap();
    for directive in [
        "default-src 'none'",
        "script-src 'self'",
        "connect-src 'self'",
        "img-src 'self' data:",
        "object-src 'none'",
        "base-uri 'none'",
        "form-action 'none'",
        "frame-ancestors 'none'",
        "frame-src 'none'",
    ] {
        assert!(csp.split(';').any(|part| part.trim() == directive));
    }
    assert!(!csp.contains("unsafe-inline"));
    assert!(!csp.contains("unsafe-eval"));
}

async fn error(response: Response, status: StatusCode, code: &str) {
    privacy(&response);
    assert_eq!(response.status(), status);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"error":{"code":code}})
    );
}

#[test]
fn private_bindings_only() {
    for address in [
        "127.0.0.1",
        "127.0.0.2",
        "::1",
        "100.64.0.0",
        "100.127.255.255",
        "fd7a:115c:a1e0::1",
    ] {
        validate_browse_bind(address.parse().unwrap()).unwrap();
    }
    for address in [
        "0.0.0.0",
        "::",
        "192.168.1.1",
        "10.0.0.1",
        "8.8.8.8",
        "100.63.255.255",
        "100.128.0.0",
        "fd7a:115c:a1e1::1",
        "::ffff:127.0.0.1",
    ] {
        assert!(
            validate_browse_bind(address.parse().unwrap()).is_err(),
            "{address}"
        );
        let output = Command::new(env!("CARGO_BIN_EXE_spool-server"))
            .env_remove("DATABASE_URL")
            .args(["browse", "--bind", address])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("loopback or a Tailscale IP"));
    }
}

#[tokio::test]
async fn static_files_and_origin_guards() {
    let assets = Assets::new();
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://unused@127.0.0.1:1/unused")
        .unwrap();
    let missing = browse_router(pool.clone(), assets.0.join("missing")).unwrap_err();
    assert!(missing.to_string().contains("browse assets missing"));
    let (url, task) = serve(browse_router(pool.clone(), &assets.0).unwrap()).await;
    let client = Client::new();
    for (path, content_type) in [
        ("/", "text/html"),
        ("/index.html", "text/html"),
        ("/assets/test.js", "text/javascript"),
    ] {
        let response = client.get(format!("{url}{path}")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with(content_type)
        );
        privacy(&response);
        assert!(!response.text().await.unwrap().is_empty());
    }
    for path in [
        "/private.txt",
        "/favicon.ico",
        "/assets/missing.js",
        "/assets/%2e%2e%2fprivate.txt",
        "/v1/sync/sessions",
    ] {
        let response = client.get(format!("{url}{path}")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        privacy(&response);
    }
    for (name, value) in [
        ("origin", "https://evil.invalid"),
        ("origin", "null"),
        ("origin", "http://127.0.0.1:1"),
        ("sec-fetch-site", "cross-site"),
        ("sec-fetch-site", "same-site"),
        ("host", "evil.invalid"),
        ("host", "evil.localhost"),
        ("host", "localhost.evil.invalid"),
        ("host", "192.168.1.1"),
        ("host", "127.0.0.1:bad"),
        ("host", "evil@127.0.0.1"),
    ] {
        let response = client
            .get(format!("{url}/api/sessions"))
            .header(name, value)
            .send()
            .await
            .unwrap();
        error(response, StatusCode::FORBIDDEN, "forbidden_origin").await;
    }
    for host in [
        "localhost",
        "127.0.0.2",
        "100.64.1.2:8090",
        "[::1]:8090",
        "[fd7a:115c:a1e0::1]:8090",
    ] {
        let response = client
            .get(format!("{url}/"))
            .header("host", host)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{host}");
    }
    let response = client
        .post(format!("{url}/api/sessions"))
        .send()
        .await
        .unwrap();
    error(
        response,
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
    )
    .await;
    task.abort();
    let _ = task.await;
    pool.close().await;
}

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn cli_without_migrations(database_url: &str, pool: &PgPool, assets: &Assets) {
    let missing = Command::new(env!("CARGO_BIN_EXE_spool-server"))
        .env("DATABASE_URL", database_url)
        .args([
            "browse",
            "--assets",
            assets.0.join("missing").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("browse assets missing"));
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port().to_string();
    drop(socket);
    let mut child = Process(
        Command::new(env!("CARGO_BIN_EXE_spool-server"))
            .env("DATABASE_URL", database_url)
            .env("BIND_ADDR", "0.0.0.0")
            .env("PORT", "invalid")
            .args([
                "browse",
                "--port",
                &port,
                "--assets",
                assets.0.to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let client = Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let url = format!("http://127.0.0.1:{port}");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "browse exited before listening"
            );
            if let Ok(response) = client.get(&url).send().await {
                assert_eq!(response.status(), StatusCode::OK);
                privacy(&response);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    error(
        client
            .get(format!("{url}/api/sessions"))
            .send()
            .await
            .unwrap(),
        StatusCode::SERVICE_UNAVAILABLE,
        "database_unavailable",
    )
    .await;
    let tables: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE schemaname = 'public'")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(
        tables, 0,
        "browse must not create archive or migration tables"
    );
}

#[tokio::test]
async fn postgres_browse_contract() {
    let Ok(database_url) = std::env::var("SPOOL_TEST_DATABASE_URL") else {
        eprintln!(
            "PostgreSQL browse regression skipped: set SPOOL_TEST_DATABASE_URL (requires a disposable PostgreSQL superuser)"
        );
        return;
    };
    let admin = connect(&database_url).await.unwrap();
    let database = format!("spool_test_{}", Uuid::new_v4().simple());
    let reader_role = format!("spool_reader_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await
        .unwrap();
    let work_database = database.clone();
    let work_role = reader_role.clone();
    // Setup also belongs inside the task: failed provisioning must not leak a role or DB.
    let result = tokio::spawn(async move {
        let mut database_url = reqwest::Url::parse(&database_url).unwrap();
        database_url.set_path(&work_database);
        let pool = connect(database_url.as_str()).await.unwrap();
        sqlx::query(&format!(
            "ALTER DATABASE {work_database} SET plan_cache_mode = force_generic_plan"
        ))
        .execute(&pool)
        .await
        .unwrap();
        // Make CONNECT and schema USAGE depend on the script, not ambient PUBLIC grants.
        sqlx::query(&format!(
            "REVOKE CONNECT ON DATABASE {work_database} FROM PUBLIC"
        ))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("REVOKE USAGE ON SCHEMA public FROM PUBLIC")
            .execute(&pool)
            .await
            .unwrap();
        let assets = Assets::new();
        cli_without_migrations(database_url.as_str(), &pool, &assets).await;
        let provisioning =
            include_str!("../../deploy/reader-role.sql").replace("spool_reader", &work_role);
        provisioning_error(&pool, &provisioning, "42P01").await;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)")
            .bind(&work_role)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!exists, "failed provisioning must roll back the cluster-scoped role");
        migrate(&pool).await.unwrap();
        pool.execute(provisioning.as_str()).await.unwrap();
        let restricted: bool = sqlx::query_scalar("SELECT NOT (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole OR rolinherit OR rolreplication OR rolbypassrls) FROM pg_roles WHERE rolname = $1")
            .bind(&work_role).fetch_one(&pool).await.unwrap();
        assert!(restricted, "provisioning must create a restricted NOLOGIN role");
        sqlx::query(&format!(
            "ALTER ROLE {work_role} LOGIN PASSWORD 'synthetic-reader-only'"
        ))
        .execute(&pool)
        .await
        .unwrap();
        provisioning_error(&pool, &provisioning, "42710").await;
        database_url.set_username(&work_role).unwrap();
        database_url.set_password(Some("synthetic-reader-only")).unwrap();
        reader_privileges(database_url.as_str(), &work_role).await;
        let read = connect_browse(database_url.as_str()).await.unwrap();
        let (url, task) = serve(browse_router(read.clone(), &assets.0).unwrap()).await;
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap();
        let health = client.get(format!("{url}/healthz")).send().await.unwrap();
        privacy(&health);
        assert_eq!(health.status(), StatusCode::NO_CONTENT);
        assert!(health.bytes().await.unwrap().is_empty());
        assert_eq!(get(&client, &format!("{url}/api/sessions")).await["sessions"], json!([]));
        seed(&pool).await;
        api_contract(&client, &url, &pool).await;
        snapshot_consistency(&client, &url, &pool).await;
        limits(&client, &url, &pool).await;
        let (public_url, public_task) = serve(router(pool.clone())).await;
        let response = client
            .get(format!("{public_url}/api/sessions"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "browse must never be on the public ingestion listener"
        );
        public_task.abort();
        let _ = public_task.await;
        read.close().await;
        error(
            client.get(format!("{url}/healthz")).send().await.unwrap(),
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unavailable",
        )
        .await;
        error(
            client
                .get(format!("{url}/api/sessions"))
                .send()
                .await
                .unwrap(),
            StatusCode::SERVICE_UNAVAILABLE,
            "database_unavailable",
        )
        .await;
        task.abort();
        let _ = task.await;
        pool.close().await;
    })
    .await;
    let drop_database = sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .execute(&admin)
        .await;
    let drop_role = sqlx::query(&format!("DROP ROLE IF EXISTS {reader_role}"))
        .execute(&admin)
        .await;
    admin.close().await;
    drop_database.unwrap();
    drop_role.unwrap();
    result.unwrap();
}

async fn provisioning_error(pool: &PgPool, script: &str, code: &str) {
    let mut connection = pool.acquire().await.unwrap();
    let error = connection.execute(script).await.unwrap_err();
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .unwrap();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some(code)
    );
}

async fn read_only_connections(pool: &PgPool) {
    let first = pool.acquire().await.unwrap();
    let second = pool.acquire().await.unwrap();
    for mut connection in [first, second] {
        let readonly: String = sqlx::query_scalar("SHOW default_transaction_read_only")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(readonly, "on");
        for statement in ["CREATE TABLE forbidden (id int)", "DELETE FROM sources"] {
            let error = sqlx::query(statement)
                .execute(&mut *connection)
                .await
                .unwrap_err();
            assert_eq!(
                error.as_database_error().unwrap().code().as_deref(),
                Some("25006")
            );
        }
    }
}

async fn reader_privileges(database_url: &str, role: &str) {
    // Use a normal pool so the role's own default is checked, not browse's override.
    let pool = connect(database_url).await.unwrap();
    read_only_connections(&pool).await;
    let mut connection = pool.acquire().await.unwrap();
    let identity: (String, String) =
        sqlx::query_as("SELECT session_user::text, current_user::text")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
    assert_eq!(identity, (role.to_owned(), role.to_owned()));
    sqlx::query("SET default_transaction_read_only = off")
        .execute(&mut *connection)
        .await
        .unwrap();
    let readonly: String = sqlx::query_scalar("SHOW transaction_read_only")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(readonly, "off");
    for statement in [
        "INSERT INTO sources (id, label, token_hash) VALUES (gen_random_uuid(), 'forbidden', decode(repeat('42',32),'hex'))",
        "UPDATE sources SET label = 'forbidden'",
        "DELETE FROM sources",
        "TRUNCATE sources CASCADE",
        "INSERT INTO sessions (source_id, session_id, revision, format) VALUES ('00000000-0000-0000-0000-000000000001', 'forbidden', 1, 'opencode.v2')",
        "UPDATE sessions SET revision = 99",
        "DELETE FROM sessions",
        "TRUNCATE sessions CASCADE",
        "INSERT INTO messages (source_id, session_id, message_id, position, payload, payload_hash) VALUES ('00000000-0000-0000-0000-000000000001', 'same', 'forbidden', 0, '{}'::json, decode(repeat('42',32),'hex'))",
        "UPDATE messages SET payload = '{}'::json",
        "DELETE FROM messages",
        "TRUNCATE messages",
        "CREATE TABLE public.forbidden (id int)",
        "CREATE SCHEMA forbidden",
        "ALTER TABLE sources ADD COLUMN forbidden int",
        "DROP TABLE messages",
        "SELECT token_hash FROM sources",
        "SELECT created_at FROM sources",
        "SELECT last_seen_at FROM sources",
        "SELECT revoked_at FROM sources",
        "SELECT format FROM sessions",
        "SELECT info_hash FROM sessions",
        "SELECT upstream_version FROM sessions",
        "SELECT collector_version FROM sessions",
        "SELECT captured_at FROM sessions",
        "SELECT message_id FROM messages",
        "SELECT payload_hash FROM messages",
        "SELECT * FROM sources",
        "SELECT * FROM sessions",
        "SELECT * FROM messages",
        "SELECT * FROM _sqlx_migrations",
    ] {
        let error = sqlx::query(statement)
            .execute(&mut *connection)
            .await
            .expect_err(statement);
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("42501"),
            "privileges, not a read-only transaction, must deny: {statement}"
        );
    }
    drop(connection);
    pool.close().await;
}

async fn insert_session(pool: &PgPool, source: Uuid, id: &str, info: &str) {
    sqlx::query("INSERT INTO sessions (source_id, session_id, revision, format, info, info_hash, upstream_version, collector_version, captured_at, received_at) VALUES ($1, $2, 1, 'opencode.v2', $3::json, decode(repeat('ab',32),'hex'), 'test','test','2026-11-08T00:00:00Z','2026-11-08T01:00:00Z')")
        .bind(source).bind(id).bind(info).execute(pool).await.unwrap();
}

async fn seed(pool: &PgPool) {
    for (source, label) in [(A, "machine A"), (B, "machine B")] {
        sqlx::query("INSERT INTO sources (id, label, token_hash) VALUES ($1,$2,decode(repeat('42',32),'hex'))")
            .bind(source).bind(label).execute(pool).await.unwrap();
    }
    insert_session(pool, A, "same", RAW_INFO).await;
    for (source, id) in [(A, "aaa"), (A, "zzz"), (B, "same")] {
        insert_session(
            pool,
            source,
            id,
            &json!({"id":id,"source":source}).to_string(),
        )
        .await;
    }
    spool_server::admin::purge_session(pool, A, "purged")
        .await
        .unwrap();
    for (position, id, payload) in [
        (0, "m3", RAW_MESSAGE.to_owned()),
        (1, "m1", json!({"id":"m1","future":true}).to_string()),
        (2, "m2", json!({"id":"m2","text":"third"}).to_string()),
    ] {
        sqlx::query("INSERT INTO messages (source_id,session_id,message_id,position,payload,payload_hash) VALUES ($1,'same',$2,$3,$4::json,decode(repeat('ab',32),'hex'))")
            .bind(A).bind(id).bind(position).bind(payload).execute(pool).await.unwrap();
    }
}

async fn get(client: &Client, url: &str) -> Value {
    let response = client.get(url).send().await.unwrap();
    privacy(&response);
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

async fn api_contract(client: &Client, url: &str, pool: &PgPool) {
    for (source, id, info) in [
        (
            A,
            "aaa",
            json!({"id":"aaa","title":"An ancient conversation","time":{"updated":1}}),
        ),
        (
            A,
            "zzz",
            json!({"id":"zzz","title":"Needle 100%_ match","location":{"directory":"/synthetic/projects/needle"},"time":{"updated":2000000000000_i64}}),
        ),
        (
            B,
            "same",
            json!({"id":"same","source":B,"title":"Other match","time":{"updated":2000000000000_i64}}),
        ),
    ] {
        sqlx::query("UPDATE sessions SET info = $3::json WHERE source_id = $1 AND session_id = $2")
            .bind(source)
            .bind(id)
            .bind(info.to_string())
            .execute(pool)
            .await
            .unwrap();
    }
    let response = client
        .get(format!("{url}/api/sessions"))
        .header("origin", url)
        .header("sec-fetch-site", "same-origin")
        .send()
        .await
        .unwrap();
    privacy(&response);
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.unwrap();
    assert!(
        text.contains(RAW_INFO),
        "JSON metadata must retain its raw representation"
    );
    let all: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(all["next_cursor"], Value::Null);
    let rows = all["sessions"].as_array().unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| (
                row["source_id"].as_str().unwrap(),
                row["session_id"].as_str().unwrap()
            ))
            .collect::<Vec<_>>(),
        [
            (B.to_string().as_str(), "same"),
            (A.to_string().as_str(), "zzz"),
            (A.to_string().as_str(), "same"),
            (A.to_string().as_str(), "aaa")
        ]
    );
    assert_eq!(
        all["sources"],
        json!([
            {"source_id":A,"source_label":"machine A"},
            {"source_id":B,"source_label":"machine B"}
        ])
    );
    for (query, expected) in [
        ("ANCIENT", "aaa"),
        ("100%_", "zzz"),
        ("SYNTHETIC/projects", "zzz"),
        ("MACHINE B", "same"),
    ] {
        let response = client
            .get(format!("{url}/api/sessions"))
            .query(&[("q", query), ("limit", "1")])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page: Value = response.json().await.unwrap();
        assert_eq!(page["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(page["sessions"][0]["session_id"], expected);
        assert_eq!(
            page["sources"], all["sources"],
            "filters must not hide source choices"
        );
    }
    let filtered = get(client, &format!("{url}/api/sessions?source={B}&q=needle")).await;
    assert_eq!(filtered["sessions"], json!([]));
    let filtered = get(client, &format!("{url}/api/sessions?source={A}")).await;
    assert_eq!(filtered["sessions"].as_array().unwrap().len(), 3);
    for row in rows {
        assert_eq!(
            row.as_object().unwrap().len(),
            6,
            "only contract fields; no token_hash or message payloads"
        );
        assert_eq!(row["revision"], "1");
        DateTime::parse_from_rfc3339(row["received_at"].as_str().unwrap()).unwrap();
        assert_eq!(
            row["source_label"],
            if row["source_id"] == A.to_string() {
                "machine A"
            } else {
                "machine B"
            }
        );
    }
    let mut paged = Vec::new();
    let mut next = None;
    let mut list_cursor = String::new();
    loop {
        let mut request = client
            .get(format!("{url}/api/sessions"))
            .query(&[("limit", "1")]);
        if let Some(cursor) = &next {
            request = request.query(&[("cursor", cursor)]);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page: Value = response.json().await.unwrap();
        paged.extend(page["sessions"].as_array().unwrap().iter().cloned());
        next = page["next_cursor"].as_str().map(str::to_owned);
        if let Some(cursor) = &next {
            list_cursor.clone_from(cursor);
        } else {
            break;
        }
    }
    assert_eq!(
        paged, *rows,
        "keyset must cross source boundaries without dropping colliding session IDs"
    );
    let path = format!("{url}/api/sources/{A}/sessions/same");
    let response = client
        .get(&path)
        .query(&[("limit", "1")])
        .send()
        .await
        .unwrap();
    privacy(&response);
    let text = response.text().await.unwrap();
    assert!(text.contains(RAW_INFO));
    assert!(text.contains(RAW_MESSAGE));
    let first: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(first.as_object().unwrap().len(), 8);
    assert_eq!(first["info"]["unknown"]["text"], "x\0y");
    assert_eq!(first["messages"][0]["parts"][0]["text"], "a\0b");
    assert_eq!(first["messages"][0]["id"], "m3");
    let cursor = first["next_cursor"].as_str().unwrap();
    let second = get(client, &format!("{path}?limit=1&cursor={cursor}")).await;
    assert_eq!(second["messages"][0]["id"], "m1");
    let last = get(
        client,
        &format!(
            "{path}?limit=1&cursor={}",
            second["next_cursor"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(last["messages"][0]["id"], "m2");
    assert_eq!(last["next_cursor"], Value::Null);
    let other = get(client, &format!("{url}/api/sources/{B}/sessions/same")).await;
    assert_eq!(other["info"]["source"], B.to_string());
    assert_eq!(other["messages"], json!([]));
    assert_eq!(other["next_cursor"], Value::Null);
    for id in ["purged", "missing"] {
        error(
            client
                .get(format!("{url}/api/sources/{A}/sessions/{id}"))
                .send()
                .await
                .unwrap(),
            StatusCode::NOT_FOUND,
            "not_found",
        )
        .await;
    }
    for endpoint in [
        format!("{url}/api/sources/{B}/sessions/same"),
        format!("{url}/api/sources/{A}/sessions/zzz"),
        format!("{url}/api/sessions"),
    ] {
        error(
            client
                .get(endpoint)
                .query(&[("cursor", cursor)])
                .send()
                .await
                .unwrap(),
            StatusCode::BAD_REQUEST,
            "malformed_request",
        )
        .await;
    }
    error(
        client
            .get(&path)
            .query(&[("cursor", &list_cursor)])
            .send()
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
        "malformed_request",
    )
    .await;
    for endpoint in [format!("{url}/api/sessions"), path.clone()] {
        for query in [
            "limit=0",
            "limit=501",
            "limit=-1",
            "limit=nope",
            "limit=1&limit=2",
            "cursor=",
            "cursor=invalid",
            "cursor=e30",
            "cursor=AAAA",
        ] {
            error(
                client
                    .get(format!("{endpoint}?{query}"))
                    .send()
                    .await
                    .unwrap(),
                StatusCode::BAD_REQUEST,
                "malformed_request",
            )
            .await;
        }
    }
    for limit in ["201", "500"] {
        error(
            client
                .get(&path)
                .query(&[("limit", limit)])
                .send()
                .await
                .unwrap(),
            StatusCode::BAD_REQUEST,
            "malformed_request",
        )
        .await;
    }
    let forged = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"kind":"messages_v1","source_id":A,"session_id":"same","revision":1,"position":-1})).unwrap());
    error(
        client
            .get(&path)
            .query(&[("cursor", forged)])
            .send()
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
        "malformed_request",
    )
    .await;
    error(
        client
            .get(format!("{url}/api/sources/not-a-uuid/sessions/same"))
            .send()
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
        "malformed_request",
    )
    .await;
    sqlx::query("UPDATE sessions SET revision = 2 WHERE source_id = $1 AND session_id = 'same'")
        .bind(A)
        .execute(pool)
        .await
        .unwrap();
    let changed = client
        .get(&path)
        .query(&[("cursor", cursor)])
        .send()
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::CONFLICT);
    privacy(&changed);
    assert_eq!(
        changed.json::<Value>().await.unwrap(),
        json!({"error":{"code":"revision_conflict","current_revision":"2"}})
    );
    assert_eq!(get(client, &path).await["revision"], "2");
    let untouched: bool = sqlx::query_scalar("SELECT bool_and(last_seen_at IS NULL) FROM sources")
        .fetch_one(pool)
        .await
        .unwrap();
    assert!(
        untouched,
        "browse must not update source last-seen metadata"
    );
}

async fn snapshot_consistency(client: &Client, url: &str, pool: &PgPool) {
    let mut writer = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE messages IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *writer)
        .await
        .unwrap();
    let request = client
        .get(format!("{url}/api/sources/{A}/sessions/same"))
        .send();
    let request = tokio::spawn(async move { request.await.unwrap() });
    // Stop the transcript between its metadata and message queries, then atomically
    // replace both. The reader must retain the pre-commit snapshot for both pieces.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE 'SELECT position, payload::text%')")
                .fetch_one(pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    sqlx::query("UPDATE sessions SET revision = 3, info = '{\"id\":\"same\",\"generation\":3}'::json WHERE source_id = $1 AND session_id = 'same'")
        .bind(A).execute(&mut *writer).await.unwrap();
    sqlx::query("UPDATE messages SET payload = '{\"id\":\"new\",\"generation\":3}'::json WHERE source_id = $1 AND session_id = 'same'")
        .bind(A).execute(&mut *writer).await.unwrap();
    writer.commit().await.unwrap();
    let response = request.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let before: Value = response.json().await.unwrap();
    assert_eq!(before["revision"], "2");
    assert_eq!(
        before["info"],
        serde_json::from_str::<Value>(RAW_INFO).unwrap()
    );
    assert_eq!(before["messages"][0]["id"], "m3");
    let after = get(client, &format!("{url}/api/sources/{A}/sessions/same")).await;
    assert_eq!(after["revision"], "3");
    assert_eq!(after["info"]["generation"], 3);
    assert_eq!(after["messages"][0]["generation"], 3);
    let inventory = get(client, &format!("{url}/api/sessions?source={A}")).await;
    let updated = inventory["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["session_id"] == "same")
        .unwrap();
    assert_eq!(updated["revision"], "3");
    assert_eq!(updated["info"]["generation"], 3);
}

async fn limits(client: &Client, url: &str, pool: &PgPool) {
    sqlx::query("INSERT INTO sessions (source_id, session_id, revision, format, info, info_hash, upstream_version, collector_version, captured_at) SELECT $1, 'limit_' || lpad(n::text,3,'0'), 1, 'opencode.v2', json_build_object('id', 'limit_' || lpad(n::text,3,'0')), decode(repeat('ab',32),'hex'), 'test','test',now() FROM generate_series(0,204) AS n")
        .bind(B).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO messages (source_id,session_id,message_id,position,payload,payload_hash) SELECT $1,'limit_000','m' || n,n,json_build_object('id','m' || n),decode(repeat('ab',32),'hex') FROM generate_series(0,204) AS n")
        .bind(B).execute(pool).await.unwrap();
    let sessions = get(client, &format!("{url}/api/sessions")).await;
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 50);
    assert!(sessions["next_cursor"].is_string());
    let max = get(client, &format!("{url}/api/sessions?limit=500")).await;
    assert_eq!(max["sessions"].as_array().unwrap().len(), 209);
    assert_eq!(max["next_cursor"], Value::Null);
    let path = format!("{url}/api/sources/{B}/sessions/limit_000");
    let messages = get(client, &path).await;
    assert_eq!(messages["messages"].as_array().unwrap().len(), 100);
    assert!(messages["next_cursor"].is_string());
    let max = get(client, &format!("{path}?limit=200")).await;
    assert_eq!(max["messages"].as_array().unwrap().len(), 200);
    let tail = get(
        client,
        &format!("{path}?cursor={}", max["next_cursor"].as_str().unwrap()),
    )
    .await;
    assert_eq!(tail["messages"].as_array().unwrap().len(), 5);
    assert_eq!(tail["messages"][0]["id"], "m200");
    assert_eq!(tail["next_cursor"], Value::Null);
}
