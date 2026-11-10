use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, http::StatusCode as AxumStatus, response::IntoResponse, routing::get as route_get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::DateTime;
use reqwest::{
    Client, Response, StatusCode,
    header::{HeaderMap, HeaderValue},
};
use ring::{
    rand::SystemRandom,
    rsa::PublicKeyComponents,
    signature::{RSA_PKCS1_SHA256, RsaKeyPair},
};
use serde_json::{Value, json};
use spool_server::{Access, Reader, connect, connect_read, migrate, router};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::task::JoinHandle;
use uuid::Uuid;

const A: Uuid = Uuid::from_u128(1);
const B: Uuid = Uuid::from_u128(2);
const RAW_INFO: &str = r#"{ "id":"same", "unknown":{"text":"x\u0000y","number":1e3}, "html":"<script>alert(1)</script>" }"#;
const RAW_MESSAGE: &str =
    r#"{ "id":"m3", "parts":[{"type":"future","text":"a\u0000b"}], "number":1e3 }"#;
const AUDIENCE: &str = "syntheticaudience";
const KID: &str = "synthetic-key";

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

/// A synthetic Cloudflare Access team: its certificate endpoint and assertion signer.
struct Team {
    issuer: String,
    key: RsaKeyPair,
    fetches: Arc<AtomicUsize>,
    down: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Team {
    async fn start() -> Self {
        // Test-only key; it authorizes nothing outside these tests.
        let key = RsaKeyPair::from_pkcs8(include_bytes!("fixtures/access-test-only.pk8")).unwrap();
        let public = PublicKeyComponents::<Vec<u8>>::from(key.public());
        let certs = json!({"keys": [
            {"kid": "unrelated", "kty": "EC", "crv": "P-256", "x": "AA", "y": "AA"},
            {"kid": KID, "kty": "RSA", "alg": "RS256", "use": "sig",
             "n": URL_SAFE_NO_PAD.encode(&public.n), "e": URL_SAFE_NO_PAD.encode(&public.e)},
        ]});
        let fetches = Arc::new(AtomicUsize::new(0));
        let down = Arc::new(AtomicBool::new(false));
        let (counter, unavailable) = (fetches.clone(), down.clone());
        let (issuer, task) = serve(axum::Router::new().route(
            "/cdn-cgi/access/certs",
            route_get(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                let response = if unavailable.load(Ordering::SeqCst) {
                    AxumStatus::SERVICE_UNAVAILABLE.into_response()
                } else {
                    Json(certs.clone()).into_response()
                };
                async move { response }
            }),
        ))
        .await;
        Self {
            issuer,
            key,
            fetches,
            down,
            task,
        }
    }

    fn access(&self) -> Access {
        Access::new(&self.issuer, AUDIENCE).unwrap()
    }

    fn sign(&self, header: Value, claims: Value) -> String {
        let encode = |value: &Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap());
        let signed = format!("{}.{}", encode(&header), encode(&claims));
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signed.as_bytes(),
                &mut signature,
            )
            .unwrap();
        format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature))
    }

    fn claims(&self) -> Value {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        json!({"iss": self.issuer, "aud": [AUDIENCE], "exp": now + 600, "nbf": now - 5,
               "iat": now - 5, "email": "owner@example.invalid", "type": "app"})
    }

    fn assertion(&self) -> String {
        self.sign(
            json!({"alg": "RS256", "kid": KID, "typ": "JWT"}),
            self.claims(),
        )
    }

    fn client(&self) -> Client {
        let mut headers = HeaderMap::new();
        headers.insert(
            "cf-access-jwt-assertion",
            HeaderValue::from_str(&self.assertion()).unwrap(),
        );
        Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap()
    }
}

impl Drop for Team {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn app(pool: &PgPool, read: &PgPool, access: Access, assets: &Assets) -> axum::Router {
    router(
        pool.clone(),
        read.clone(),
        Some(Reader {
            access,
            assets: assets.0.clone(),
        }),
    )
    .unwrap()
}

fn privacy(response: &Response) {
    let headers = response.headers();
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["x-frame-options"], "DENY");
    assert_eq!(headers["cross-origin-resource-policy"], "same-origin");
    assert_eq!(headers["strict-transport-security"], "max-age=31536000");
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
fn access_configuration() {
    for issuer in [
        "https://team.cloudflareaccess.com",
        "https://team.cloudflareaccess.com/",
        "http://127.0.0.1:9000",
        "http://[::1]:9000",
    ] {
        Access::new(issuer, AUDIENCE).unwrap();
    }
    for issuer in [
        "http://team.cloudflareaccess.com",
        "http://192.168.1.1",
        "https://team.cloudflareaccess.com/path",
        "https://team.cloudflareaccess.com/?q=1",
        "https://user@team.cloudflareaccess.com",
        "team.cloudflareaccess.com",
    ] {
        assert!(Access::new(issuer, AUDIENCE).is_err(), "{issuer}");
    }
    for audience in ["", "not-alphanumeric", &"a".repeat(257)] {
        assert!(
            Access::new("https://team.cloudflareaccess.com", audience).is_err(),
            "{audience}"
        );
    }
}

#[tokio::test]
async fn access_assertions_and_origin_guards() {
    let assets = Assets::new();
    let team = Team::start().await;
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(500))
        .connect_lazy("postgres://unused@127.0.0.1:1/unused")
        .unwrap();
    let missing = router(
        pool.clone(),
        pool.clone(),
        Some(Reader {
            access: team.access(),
            assets: assets.0.join("missing"),
        }),
    )
    .unwrap_err();
    assert!(missing.to_string().contains("web assets missing"));
    let (url, task) = serve(app(&pool, &pool, team.access(), &assets)).await;
    let client = team.client();
    for (path, content_type) in [
        ("/", "text/html"),
        ("/index.html", "text/html"),
        ("/assets/test.js", "text/javascript"),
    ] {
        let response = client.get(format!("{url}{path}")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
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
    ] {
        let response = client.get(format!("{url}{path}")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        privacy(&response);
    }

    let plain = Client::new();
    let header = json!({"alg": "RS256", "kid": KID, "typ": "JWT"});
    let valid = team.assertion();
    let mut tampered: Vec<_> = valid.split('.').map(str::to_owned).collect();
    let mut forged = team.claims();
    forged["email"] = json!("attacker@example.invalid");
    tampered[1] = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap());
    let claims = |change: &dyn Fn(&mut Value)| {
        let mut claims = team.claims();
        change(&mut claims);
        team.sign(header.clone(), claims)
    };
    let now = team.claims()["iat"].as_u64().unwrap();
    let mut rejected = vec![
        String::new(),
        "not-a-jwt".to_owned(),
        tampered.join("."),
        format!("{valid}.extra"),
        claims(&|claims| claims["aud"] = json!(["another"])),
        claims(&|claims| claims["aud"] = json!([])),
        claims(&|claims| claims["iss"] = json!("https://evil.cloudflareaccess.com")),
        claims(&|claims| claims["exp"] = json!(now - 120)),
        claims(&|claims| claims["nbf"] = json!(now + 600)),
        claims(&|claims| {
            claims.as_object_mut().unwrap().remove("exp");
        }),
        team.sign(json!({"alg": "RS256", "kid": "unknown"}), team.claims()),
        team.sign(json!({"alg": "RS384", "kid": KID}), team.claims()),
        team.sign(json!({"alg": "RS256"}), team.claims()),
    ];
    let unsigned: Vec<_> = valid.split('.').collect();
    rejected.push(format!(
        "{}.{}.",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none","kid":"synthetic-key"}"#),
        unsigned[1]
    ));
    for path in [
        "/",
        "/api",
        "/api/sessions",
        "//api/sessions",
        "/API/sessions",
        "/assets/test.js",
        "/assets/%2e%2e%2fprivate.txt",
        "/v1x",
        "/missing",
    ] {
        error(
            plain.get(format!("{url}{path}")).send().await.unwrap(),
            StatusCode::FORBIDDEN,
            "forbidden",
        )
        .await;
        for assertion in &rejected {
            error(
                plain
                    .get(format!("{url}{path}"))
                    .header("cf-access-jwt-assertion", assertion)
                    .send()
                    .await
                    .unwrap(),
                StatusCode::FORBIDDEN,
                "forbidden",
            )
            .await;
        }
    }
    let mut duplicate = HeaderMap::new();
    duplicate.append("cf-access-jwt-assertion", valid.parse().unwrap());
    duplicate.append("cf-access-jwt-assertion", valid.parse().unwrap());
    error(
        plain
            .get(format!("{url}/"))
            .headers(duplicate)
            .send()
            .await
            .unwrap(),
        StatusCode::FORBIDDEN,
        "forbidden",
    )
    .await;
    // A single audience string is valid, and expiry/not-before allow 30s of skew.
    for assertion in [
        claims(&|claims| claims["aud"] = json!(AUDIENCE)),
        claims(&|claims| claims["exp"] = json!(now - 10)),
        claims(&|claims| claims["nbf"] = json!(now + 10)),
    ] {
        let response = plain
            .get(format!("{url}/"))
            .header("cf-access-jwt-assertion", assertion)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    // Keys were fetched once; unknown key IDs cannot force refetches within the interval,
    // and cached keys keep working while the certificate endpoint is down.
    assert_eq!(team.fetches.load(Ordering::SeqCst), 1);
    team.down.store(true, Ordering::SeqCst);
    let unknown = team.sign(json!({"alg": "RS256", "kid": "rotated"}), team.claims());
    for _ in 0..5 {
        let response = plain
            .get(format!("{url}/"))
            .header("cf-access-jwt-assertion", &unknown)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(team.fetches.load(Ordering::SeqCst), 1);
    let response = client.get(format!("{url}/")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    team.down.store(false, Ordering::SeqCst);

    // Machine routes never consult Access.
    for (method, path, status) in [
        ("GET", "/healthz", StatusCode::SERVICE_UNAVAILABLE),
        ("GET", "/v1/sync/sessions", StatusCode::UNAUTHORIZED),
        ("POST", "/v1/agent/find", StatusCode::UNAUTHORIZED),
        ("GET", "/v1/agent/find", StatusCode::UNAUTHORIZED),
        ("GET", "/v1", StatusCode::NOT_FOUND),
        ("GET", "/v1/unknown", StatusCode::NOT_FOUND),
        (
            "GET",
            "/v1/sync/sessions/x/snapshot",
            StatusCode::METHOD_NOT_ALLOWED,
        ),
    ] {
        let response = plain
            .request(method.parse().unwrap(), format!("{url}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{method} {path}");
    }
    // Each credential opens only its own routes.
    for (method, path) in [("GET", "/v1/sync/sessions"), ("POST", "/v1/agent/find")] {
        let response = client
            .request(method.parse().unwrap(), format!("{url}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "an Access assertion must not authorize {path}"
        );
    }
    let token = format!("spool_v1.{A}.{}", URL_SAFE_NO_PAD.encode([7; 32]));
    error(
        plain
            .get(format!("{url}/api/sessions"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap(),
        StatusCode::FORBIDDEN,
        "forbidden",
    )
    .await;

    let host = url.trim_start_matches("http://");
    for (name, value) in [
        ("origin", "https://evil.invalid"),
        ("origin", "null"),
        ("origin", "http://127.0.0.1:1"),
        ("origin", &format!("ftp://{host}")),
        ("sec-fetch-site", "cross-site"),
        ("sec-fetch-site", "same-site"),
    ] {
        let response = client
            .get(format!("{url}/api/sessions"))
            .header(name, value)
            .send()
            .await
            .unwrap();
        error(response, StatusCode::FORBIDDEN, "forbidden_origin").await;
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

    // Without a reachable certificate endpoint, assertions cannot be checked.
    let unreachable = Access::new("http://127.0.0.1:1", AUDIENCE).unwrap();
    let (url, task) = serve(app(&pool, &pool, unreachable, &assets)).await;
    for _ in 0..2 {
        error(
            client.get(format!("{url}/")).send().await.unwrap(),
            StatusCode::SERVICE_UNAVAILABLE,
            "access_unavailable",
        )
        .await;
    }
    task.abort();
    let _ = task.await;

    let (url, task) = serve(router(pool.clone(), pool.clone(), None).unwrap()).await;
    for path in ["/", "/index.html", "/api/sessions"] {
        let response = client.get(format!("{url}{path}")).send().await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "no reader without Access: {path}"
        );
    }
    task.abort();
    let _ = task.await;
    pool.close().await;
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
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await
        .unwrap();
    let work_database = database.clone();
    // Setup also belongs inside the task: failed provisioning must not leak a DB.
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
        migrate(&pool).await.unwrap();
        let read = connect_read(database_url.as_str()).await.unwrap();
        read_only_connections(&read).await;
        let assets = Assets::new();
        let team = Team::start().await;
        let (url, task) = serve(app(&pool, &read, team.access(), &assets)).await;
        let client = team.client();
        assert_eq!(
            get(&client, &format!("{url}/api/sessions")).await["sessions"],
            json!([])
        );
        seed(&pool).await;
        api_contract(&client, &url, &pool).await;
        snapshot_consistency(&client, &url, &pool).await;
        limits(&client, &url, &pool).await;
        read.close().await;
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
    admin.close().await;
    drop_database.unwrap();
    result.unwrap();
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
