use std::{process::Command, time::Duration};

use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spool_server::{connect, migrate, router};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

struct Harness {
    pool: PgPool,
    database_url: String,
    url: String,
    client: Client,
}

impl Harness {
    fn cli(&self, arguments: &[&str]) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_spool-server"))
            .env("DATABASE_URL", &self.database_url)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "CLI failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn source(&self, label: &str) -> (Uuid, String) {
        let credential = self.cli(&["source", "add", "--label", label]);
        (
            credential["source_id"].as_str().unwrap().parse().unwrap(),
            credential["token"].as_str().unwrap().into(),
        )
    }

    async fn post(&self, token: &str, id: &str, body: &Value) -> (StatusCode, Value) {
        let response = self
            .client
            .post(format!("{}/v1/sync/sessions/{id}/snapshot", self.url))
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = response.json().await.unwrap();
        (status, body)
    }

    async fn inventory(&self, token: &str) -> Value {
        self.client
            .get(format!("{}/v1/sync/sessions", self.url))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn messages(&self, source: Uuid, session: &str) -> Vec<(String, i32, String, String)> {
        sqlx::query_as(
            "SELECT message_id, position, payload::text, xmin::text FROM messages \
             WHERE source_id = $1 AND session_id = $2 ORDER BY position",
        )
        .bind(source)
        .bind(session)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn projection(&self, source: Uuid, session: &str) -> Value {
        let state: String = sqlx::query_scalar(
            "SELECT json_build_object('title', retrieval_title, 'repository', repository, 'activity', activity, \
             'messages', COALESCE((SELECT json_agg(json_build_object('id', message_id, 'role', retrieval_role, \
             'text', retrieval_text, 'conversation', conversation_text, 'tools', tool_text, \
             'conversation_spans', conversation_spans, 'tool_spans', tool_spans) ORDER BY position) \
             FROM messages WHERE source_id = $1 AND session_id = $2), '[]'::json))::text \
             FROM sessions WHERE source_id = $1 AND session_id = $2",
        )
        .bind(source)
        .bind(session)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        serde_json::from_str(&state).unwrap()
    }
}

fn snapshot(id: &str, base: Option<&str>, order: &[&str], upserts: Value) -> Value {
    json!({
        "base_revision": base,
        "format": "opencode.v2",
        "upstream_version": "test-upstream",
        "collector_version": "test-collector",
        "captured_at": "2026-11-08T10:00:00Z",
        "info": {"id": id, "parentID": "opaque-fork", "future": {"unknown": true}},
        "message_order": order,
        "upsert_messages": upserts,
    })
}

fn retrieval(messages: Value) -> Value {
    json!({"version": 1, "title": "Synthetic retrieval", "repository": "/synthetic/repo",
        "activity": 1_794_132_000_000_i64, "messages": messages})
}

fn projection(id: &str, text: &str) -> Value {
    let prefix = format!("excluded: {text}\n");
    let conversation = format!("conversation: {text}");
    let tools = format!("tools: {text}");
    let conversation_end = prefix.len() + conversation.len();
    let tool_start = conversation_end + 1;
    let text = format!("{prefix}{conversation}\n{tools}");
    json!({"id": id, "role": "future-generic-role", "text": text,
        "conversation": conversation, "tools": tools,
        "conversation_spans": [[prefix.len(), conversation_end]],
        "tool_spans": [[tool_start, text.len()]]})
}

#[tokio::test]
async fn postgres_http_contract() {
    let Ok(database_url) = std::env::var("SPOOL_TEST_DATABASE_URL") else {
        eprintln!(
            "PostgreSQL HTTP regression skipped: set SPOOL_TEST_DATABASE_URL (requires CREATEDB)"
        );
        return;
    };
    let admin = connect(&database_url).await.unwrap();
    let database = format!("spool_test_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(&format!(
        "ALTER DATABASE {database} SET plan_cache_mode = force_generic_plan"
    ))
    .execute(&admin)
    .await
    .unwrap();
    let mut url = reqwest::Url::parse(&database_url).unwrap();
    url.set_path(&database);
    let pool = connect(url.as_str()).await.unwrap();
    migrate(&pool).await.unwrap();
    migrate(&pool).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = router(pool.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let harness = Harness {
        pool: pool.clone(),
        database_url: url.into(),
        url: format!("http://{address}"),
        client: Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap(),
    };
    // Keep cleanup outside the task so failed assertions also drop the disposable DB.
    let result = tokio::spawn(async move {
        let (_, outage_token) = harness.source("outage");
        isolation_and_sparse_mutations(&harness).await;
        revisions_and_races(&harness).await;
        large_sessions(&harness).await;
        validation_and_raw_json(&harness).await;
        retrieval_projections(&harness).await;
        retrieval_span_validation(&harness).await;
        credentials_and_purge(&harness).await;
        admission_and_limits(&harness).await;
        retrieval_limits(&harness).await;
        harness.pool.close().await;
        assert_eq!(
            harness
                .client
                .get(format!("{}/healthz", harness.url))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        let response = harness
            .client
            .get(format!("{}/v1/sync/sessions", harness.url))
            .bearer_auth(outage_token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    })
    .await;
    server.abort();
    let _ = server.await;
    pool.close().await;
    sqlx::query(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    result.unwrap();
}

async fn isolation_and_sparse_mutations(h: &Harness) {
    assert_eq!(
        h.client
            .get(format!("{}/healthz", h.url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let (a, token_a) = h.source("machine A");
    let (b, token_b) = h.source("machine B");
    let body = snapshot(
        "same",
        None,
        &["m1", "m2", "m3"],
        json!([
            {"id": "m1", "role": "unknown-future-role", "parts": [{"type": "future", "data": [1, 2]}]},
            {"id": "m2", "text": "two"}, {"id": "m3", "text": "three"}
        ]),
    );
    assert_eq!(
        h.post(&token_a, "same", &body).await,
        (
            StatusCode::CREATED,
            json!({"revision": "1", "changed": true})
        )
    );
    assert_eq!(
        h.inventory(&token_b).await,
        json!({"sessions": [], "next_cursor": null})
    );
    let mut b_body = body.clone();
    b_body["upsert_messages"][0]["text"] = json!("other source");
    b_body["source_id"] = json!(a);
    assert_eq!(
        h.post(&token_b, "same", &b_body).await.0,
        StatusCode::CREATED
    );
    let forged = token_b.replace(&b.to_string(), &a.to_string());
    assert_eq!(
        h.post(&forged, "same", &body).await.0,
        StatusCode::UNAUTHORIZED
    );
    let initial = h.messages(a, "same").await;
    let mut noop = body.clone();
    noop["base_revision"] = json!("1");
    noop["captured_at"] = json!("2026-11-08T11:00:00Z");
    noop["collector_version"] = json!("new-collector");
    assert_eq!(
        h.post(&token_a, "same", &noop).await,
        (StatusCode::OK, json!({"revision": "2", "changed": false}))
    );
    assert_eq!(
        h.messages(a, "same").await,
        initial,
        "no-op must not rewrite any message row"
    );
    let provenance: (String, bool) = sqlx::query_as(
        "SELECT collector_version, captured_at = '2026-11-08T11:00:00Z'::timestamptz FROM sessions WHERE source_id = $1 AND session_id = 'same'",
    ).bind(a).fetch_one(&h.pool).await.unwrap();
    assert_eq!(provenance, ("new-collector".into(), true));
    let sparse = snapshot(
        "same",
        Some("2"),
        &["m3", "m2", "m1", "m4"],
        json!([
            {"id": "m2", "text": "changed"}, {"id": "m4", "text": "four"}
        ]),
    );
    assert_eq!(
        h.post(&token_a, "same", &sparse).await,
        (StatusCode::OK, json!({"revision": "3", "changed": true}))
    );
    let rows = h.messages(a, "same").await;
    assert_eq!(
        rows.iter()
            .map(|row| (row.0.as_str(), row.1))
            .collect::<Vec<_>>(),
        [("m3", 0), ("m2", 1), ("m1", 2), ("m4", 3)]
    );
    assert_eq!(
        serde_json::from_str::<Value>(&rows[1].2).unwrap()["text"],
        "changed"
    );
    assert_eq!(
        rows[2].2, initial[0].2,
        "unknown payload retained on sparse reorder"
    );
    let truncate = snapshot("same", Some("3"), &["m3", "m2"], json!([]));
    assert_eq!(h.post(&token_a, "same", &truncate).await.0, StatusCode::OK);
    let truncated = h.messages(a, "same").await;
    assert_eq!(
        truncated,
        rows[..2],
        "unchanged retained rows must not be rewritten"
    );
    let empty = snapshot("same", Some("4"), &[], json!([]));
    assert_eq!(
        h.post(&token_a, "same", &empty).await,
        (StatusCode::OK, json!({"revision": "5", "changed": true}))
    );
    assert!(h.messages(a, "same").await.is_empty());
    let mut info_only = snapshot("same", Some("5"), &[], json!([]));
    info_only["info"]["title"] = json!("new title");
    assert_eq!(
        h.post(&token_a, "same", &info_only).await,
        (StatusCode::OK, json!({"revision": "6", "changed": true}))
    );
    assert_eq!(h.messages(b, "same").await.len(), 3);
    assert_eq!(h.inventory(&token_b).await["sessions"][0]["revision"], "1");
    for id in ["aaa", "zzz"] {
        assert_eq!(
            h.post(&token_a, id, &snapshot(id, None, &[], json!([])))
                .await
                .0,
            StatusCode::CREATED
        );
    }
    let first: Value = h
        .client
        .get(format!("{}/v1/sync/sessions?limit=1", h.url))
        .bearer_auth(&token_a)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["sessions"][0]["session_id"], "aaa");
    let cursor = first["next_cursor"].as_str().unwrap();
    let second: Value = h
        .client
        .get(format!("{}/v1/sync/sessions", h.url))
        .query(&[("limit", "1"), ("cursor", cursor)])
        .bearer_auth(&token_a)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second["sessions"][0]["session_id"], "same");
    let last: Value = h
        .client
        .get(format!("{}/v1/sync/sessions", h.url))
        .query(&[
            ("limit", "1"),
            ("cursor", second["next_cursor"].as_str().unwrap()),
        ])
        .bearer_auth(&token_a)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        last,
        json!({"sessions": [{"session_id": "zzz", "revision": "1", "deleted": false}], "next_cursor": null})
    );
    assert_eq!(
        h.client
            .get(format!("{}/v1/sync/sessions", h.url))
            .query(&[("cursor", cursor)])
            .bearer_auth(&token_b)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    for query in ["limit=0", "limit=501", "limit=nope", "cursor=invalid"] {
        let response = h
            .client
            .get(format!("{}/v1/sync/sessions?{query}", h.url))
            .bearer_auth(&token_a)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "malformed_request"
        );
    }
}

async fn revisions_and_races(h: &Harness) {
    let (_, token) = h.source("races");
    let missing = snapshot("race", Some("1"), &[], json!([]));
    assert_eq!(
        h.post(&token, "race", &missing).await,
        (
            StatusCode::CONFLICT,
            json!({"error": {"code": "revision_conflict", "current_revision": null}})
        )
    );
    let create = snapshot("race", None, &[], json!([]));
    let (a, b) = tokio::join!(
        h.post(&token, "race", &create),
        h.post(&token, "race", &create)
    );
    assert!(
        (a.0 == StatusCode::CREATED && b.0 == StatusCode::CONFLICT)
            || (b.0 == StatusCode::CREATED && a.0 == StatusCode::CONFLICT)
    );
    let update = snapshot("race", Some("1"), &[], json!([]));
    let (a, b) = tokio::join!(
        h.post(&token, "race", &update),
        h.post(&token, "race", &update)
    );
    assert!(
        (a.0 == StatusCode::OK && b.0 == StatusCode::CONFLICT)
            || (b.0 == StatusCode::OK && a.0 == StatusCode::CONFLICT)
    );
    let conflict = if a.0 == StatusCode::CONFLICT {
        a.1
    } else {
        b.1
    };
    assert_eq!(
        conflict,
        json!({"error": {"code": "revision_conflict", "current_revision": "2"}})
    );
    assert_eq!(
        h.post(&token, "race", &update).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        h.post(&token, "race", &create).await.0,
        StatusCode::CONFLICT
    );
    let confirm = snapshot("race", Some("2"), &[], json!([]));
    assert_eq!(
        h.post(&token, "race", &confirm).await,
        (StatusCode::OK, json!({"revision": "3", "changed": false}))
    );
}

async fn validation_and_raw_json(h: &Harness) {
    let (source, token) = h.source("validation");
    let create = snapshot(
        "valid",
        None,
        &["old"],
        json!([{"id": "old", "text": "keep"}]),
    );
    assert_eq!(
        h.post(&token, "valid", &create).await.0,
        StatusCode::CREATED
    );
    let before = h.messages(source, "valid").await;
    let good = snapshot("valid", Some("1"), &["old"], json!([]));
    for field in [
        "base_revision",
        "format",
        "upstream_version",
        "collector_version",
        "captured_at",
        "info",
        "message_order",
        "upsert_messages",
    ] {
        let mut bad = good.clone();
        bad.as_object_mut().unwrap().remove(field);
        assert_eq!(
            h.post(&token, "valid", &bad).await.0,
            StatusCode::BAD_REQUEST,
            "missing {field}"
        );
    }
    for (field, value) in [
        ("base_revision", json!(1)),
        ("base_revision", json!("0")),
        ("base_revision", json!("9223372036854775808")),
        ("format", json!("opencode.v1")),
        ("captured_at", json!("yesterday")),
        ("message_order", Value::Null),
        ("upsert_messages", Value::Null),
    ] {
        let mut bad = good.clone();
        bad[field] = value;
        assert_eq!(
            h.post(&token, "valid", &bad).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let positional = json!([
        "1", "opencode.v2", "test-upstream", "test-collector",
        "2026-11-08T10:00:00Z", {"id": "valid"}, ["old"], []
    ]);
    assert_eq!(
        h.post(&token, "valid", &positional).await.0,
        StatusCode::BAD_REQUEST
    );
    for field in ["message_order", "upsert_messages"] {
        let mut bad = good.clone();
        bad[field] = if field == "message_order" {
            json!(vec!["old"; 100_001])
        } else {
            json!(vec![json!({"id": "old"}); 100_001])
        };
        assert_eq!(
            h.post(&token, "valid", &bad).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut array_info = good.clone();
    array_info["info"] = json!(["valid"]);
    let invalids = [
        array_info,
        snapshot("valid", Some("1"), &["old"], json!([["old"]])),
        snapshot("wrong", Some("1"), &["old"], json!([])),
        snapshot("valid", Some("1"), &["old", "old"], json!([])),
        snapshot(
            "valid",
            Some("1"),
            &["old"],
            json!([{"id": "old"}, {"id": "old"}]),
        ),
        snapshot("valid", Some("1"), &[], json!([{"id": "outside"}])),
        snapshot("valid", Some("1"), &["new"], json!([])),
        snapshot("valid", Some("1"), &["old"], json!([{"no_id": "old"}])),
        snapshot("valid", Some("1"), &["bad\0id"], json!([])),
    ];
    for bad in invalids {
        assert_eq!(
            h.post(&token, "valid", &bad).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(h.messages(source, "valid").await, before);
        assert_eq!(h.inventory(&token).await["sessions"][0]["revision"], "1");
    }
    let invalid_create = snapshot("no-row", None, &["missing"], json!([]));
    assert_eq!(
        h.post(&token, "no-row", &invalid_create).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        h.inventory(&token).await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let raw_message =
        r#"{ "id" : "nul", "parts": [{"type":"future","text":"a\u0000b"}], "unknown":1e3 }"#;
    let raw_info = r#"{"id":"raw","text":"x\u0000y"}"#;
    let raw = format!(
        r#"{{"base_revision":null,"format":"opencode.v2","upstream_version":"x","collector_version":"x","captured_at":"2026-11-08T00:00:00Z","info":{raw_info},"message_order":["nul"],"upsert_messages":[{raw_message}]}}"#
    );
    let response = h
        .client
        .post(format!("{}/v1/sync/sessions/raw/snapshot", h.url))
        .bearer_auth(&token)
        .header("content-type", "application/json; charset=utf-8")
        .body(raw)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let rows = h.messages(source, "raw").await;
    assert_eq!(rows[0].2, raw_message);
    assert_eq!(
        serde_json::from_str::<Value>(&rows[0].2).unwrap()["parts"][0]["text"],
        "a\0b"
    );
    let (info, info_hash): (String, Vec<u8>) = sqlx::query_as(
        "SELECT info::text, info_hash FROM sessions WHERE source_id = $1 AND session_id = 'raw'",
    )
    .bind(source)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert_eq!(info, raw_info);
    assert_eq!(info_hash, Sha256::digest(raw_info.as_bytes()).as_slice());
    let hash: Vec<u8> = sqlx::query_scalar(
        "SELECT payload_hash FROM messages WHERE source_id = $1 AND session_id = 'raw'",
    )
    .bind(source)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert_eq!(hash, Sha256::digest(raw_message.as_bytes()).as_slice());
}

async fn retrieval_projections(h: &Harness) {
    let (retained_source, retained_token) = h.source("repository retention");
    let mut retained = snapshot("retained", None, &["m"], json!([{"id": "m"}]));
    retained["retrieval"] = retrieval(json!([projection("m", "retained evidence")]));
    for (index, (repository, expected)) in [
        (Value::Null, Value::Null),
        (json!("example.test/first"), json!("example.test/first")),
        (Value::Null, json!("example.test/first")),
        (json!(""), json!("example.test/first")),
        (
            json!("example.test/replacement"),
            json!("example.test/replacement"),
        ),
        (Value::Null, json!("example.test/replacement")),
    ]
    .into_iter()
    .enumerate()
    {
        retained["base_revision"] = if index == 0 {
            Value::Null
        } else {
            json!(index.to_string())
        };
        retained["retrieval"]["repository"] = repository;
        assert_eq!(
            h.post(&retained_token, "retained", &retained).await.0,
            if index == 0 {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            }
        );
        assert_eq!(
            h.projection(retained_source, "retained").await["repository"],
            expected
        );
    }

    let (source, token) = h.source("retrieval");
    let (other, other_token) = h.source("independent retrieval");
    let raw = json!([{"id": "m1", "opaque": true}, {"id": "m2", "text": "raw\0NUL"}]);
    let mut body = snapshot("projected", None, &["m1", "m2"], raw.clone());
    assert_eq!(
        h.post(&token, "projected", &body).await.0,
        StatusCode::CREATED
    );
    let initial = h.messages(source, "projected").await;
    assert!(h.projection(source, "projected").await["title"].is_null());

    // A sparse first projection cannot expose a partially indexed session.
    body["base_revision"] = json!("1");
    body["upsert_messages"] = json!([raw[0]]);
    body["retrieval"] = retrieval(json!([projection("m1", "first")]));
    assert_eq!(
        h.post(&token, "projected", &body).await,
        (StatusCode::OK, json!({"revision": "2", "changed": false}))
    );
    let partial = h.messages(source, "projected").await;
    assert_eq!(partial[0].2, initial[0].2);
    assert_ne!(partial[0].3, initial[0].3);
    assert_eq!(partial[1], initial[1]);
    assert!(h.projection(source, "projected").await["title"].is_null());

    body["base_revision"] = json!("2");
    let before = h.projection(source, "projected").await;
    for messages in [
        json!([]),
        json!([projection("outside", "bad")]),
        json!([projection("m1", "a"), projection("m1", "b")]),
    ] {
        let mut invalid = body.clone();
        invalid["retrieval"]["messages"] = messages;
        assert_eq!(
            h.post(&token, "projected", &invalid).await,
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"error": {"code": "invalid_manifest"}})
            )
        );
    }
    for (field, value) in [
        ("version", json!(2)),
        ("title", json!("é".repeat(2049))),
        ("title", json!("bad\0title")),
        ("repository", json!("é".repeat(513))),
        ("repository", json!("bad\0repository")),
        ("activity", json!(1.5)),
        ("activity", json!(8_640_000_000_000_001_i64)),
    ] {
        let mut invalid = body.clone();
        invalid["retrieval"][field] = value;
        assert_eq!(
            h.post(&token, "projected", &invalid).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    for field in ["id", "role", "text", "conversation", "tools"] {
        let mut invalid = body.clone();
        invalid["retrieval"]["messages"][0][field] = json!("actual\0NUL");
        assert_eq!(
            h.post(&token, "projected", &invalid).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let mut invalid = body.clone();
    invalid["retrieval"]["messages"][0]["role"] = json!("é".repeat(129));
    assert_eq!(
        h.post(&token, "projected", &invalid).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    for invalid in [Value::Null, json!([])] {
        let mut malformed = body.clone();
        malformed["retrieval"] = invalid;
        assert_eq!(
            h.post(&token, "projected", &malformed).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut missing = snapshot("sparse-first", None, &["m1", "missing"], json!([raw[0]]));
    missing["retrieval"] = retrieval(json!([projection("m1", "first")]));
    assert_eq!(
        h.post(&token, "sparse-first", &missing).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        h.inventory(&token).await["sessions"],
        json!([{"session_id": "projected", "revision": "2", "deleted": false}])
    );
    assert_eq!(h.projection(source, "projected").await, before);
    assert_eq!(h.messages(source, "projected").await, partial);

    body["upsert_messages"] = json!([raw[1]]);
    body["retrieval"] = retrieval(json!([projection("m2", r"escaped\u0000NUL")]));
    assert_eq!(
        h.post(&token, "projected", &body).await,
        (StatusCode::OK, json!({"revision": "3", "changed": false}))
    );
    let complete = h.projection(source, "projected").await;
    assert_eq!(complete["title"], "Synthetic retrieval");
    assert_eq!(
        complete["messages"],
        json!([
            projection("m1", "first"),
            projection("m2", r"escaped\u0000NUL")
        ])
    );
    assert_eq!(
        serde_json::from_str::<Value>(&h.messages(source, "projected").await[1].2).unwrap()["text"],
        "raw\0NUL"
    );

    let mut independent = snapshot("projected", None, &["m1", "m2"], raw.clone());
    independent["retrieval"] = retrieval(json!([
        projection("m1", "other"),
        projection("m2", "source")
    ]));
    assert_eq!(
        h.post(&other_token, "projected", &independent).await.0,
        StatusCode::CREATED
    );
    let other_state = h.projection(other, "projected").await;

    body["base_revision"] = json!("3");
    body["upsert_messages"] = json!([]);
    body["retrieval"] = retrieval(json!([]));
    body["retrieval"]["title"] = json!("Metadata only");
    body["retrieval"]["repository"] = Value::Null;
    body["retrieval"]["activity"] = json!(8_640_000_000_000_000_i64);
    let rows = h.messages(source, "projected").await;
    assert_eq!(h.post(&token, "projected", &body).await.1["changed"], false);
    assert_eq!(h.messages(source, "projected").await, rows);
    let metadata = h.projection(source, "projected").await;
    assert_eq!(metadata["title"], "Metadata only");
    assert_eq!(metadata["repository"], "/synthetic/repo");
    assert_eq!(metadata["activity"], 8_640_000_000_000_000_i64);

    body["base_revision"] = json!("4");
    body["upsert_messages"] = json!([raw[0]]);
    body["retrieval"] = retrieval(json!([projection("m1", "reindexed")]));
    assert_eq!(h.post(&token, "projected", &body).await.1["changed"], false);
    let reindexed = h.messages(source, "projected").await;
    assert_eq!(reindexed[0].2, rows[0].2);
    assert_ne!(reindexed[0].3, rows[0].3);
    assert_eq!(reindexed[1], rows[1]);
    body["base_revision"] = json!("5");
    assert_eq!(h.post(&token, "projected", &body).await.1["changed"], false);
    assert_eq!(
        h.messages(source, "projected").await,
        reindexed,
        "identical projections must not rewrite rows"
    );

    body["base_revision"] = json!("6");
    body["message_order"] = json!(["m2", "m1"]);
    body["upsert_messages"] = json!([]);
    body["retrieval"] = retrieval(json!([]));
    assert_eq!(h.post(&token, "projected", &body).await.1["changed"], true);
    assert_eq!(
        h.projection(source, "projected").await["messages"],
        json!([
            projection("m2", r"escaped\u0000NUL"),
            projection("m1", "reindexed")
        ])
    );
    let reordered = h.messages(source, "projected").await;
    body["base_revision"] = json!("7");
    body["message_order"] = json!(["m2"]);
    assert_eq!(h.post(&token, "projected", &body).await.0, StatusCode::OK);
    assert_eq!(h.messages(source, "projected").await, reordered[..1]);

    body["base_revision"] = json!("8");
    body.as_object_mut().unwrap().remove("retrieval");
    assert_eq!(h.post(&token, "projected", &body).await.1["changed"], false);
    let legacy = h.projection(source, "projected").await;
    assert!(legacy["title"].is_null() && legacy["activity"].is_null());
    assert_eq!(legacy["repository"], "/synthetic/repo");
    assert_eq!(h.messages(source, "projected").await, reordered[..1]);
    body["base_revision"] = json!("9");
    body["upsert_messages"] = json!([{"id": "m2", "changed": true}]);
    assert_eq!(h.post(&token, "projected", &body).await.0, StatusCode::OK);
    assert_eq!(
        h.projection(source, "projected").await["messages"],
        json!([{"id": "m2", "role": null, "text": null, "conversation": null, "tools": null,
            "conversation_spans": null, "tool_spans": null}])
    );
    body["base_revision"] = json!("10");
    body["retrieval"] = retrieval(json!([projection("m2", "after legacy")]));
    assert_eq!(h.post(&token, "projected", &body).await.1["changed"], false);
    assert_eq!(
        h.projection(source, "projected").await["title"],
        "Synthetic retrieval"
    );
    h.cli(&["session", "purge", &source.to_string(), "projected"]);
    assert_eq!(
        h.projection(source, "projected").await,
        json!({"title": null, "repository": null, "activity": null, "messages": []})
    );
    assert_eq!(h.projection(other, "projected").await, other_state);
}

async fn retrieval_span_validation(h: &Harness) {
    let (source, token) = h.source("retrieval spans");
    let text = "😀hidden\ncafé\nignored\n工具\ncafé\ncafé";
    let cafes: Vec<_> = text
        .match_indices("café")
        .map(|(start, value)| [start, start + value.len()])
        .collect();
    let tool_start = text.find("工具").unwrap();
    let projected = json!({"id": "m", "role": "generic", "text": text,
        "conversation": "café\ncafé", "tools": "工具", "conversation_spans": [cafes[0], cafes[1]],
        "tool_spans": [[tool_start, tool_start + "工具".len()]]});
    let mut body = snapshot("span-validation", None, &["m"], json!([{"id": "m"}]));
    body["retrieval"] = retrieval(json!([projected]));
    assert_eq!(
        h.post(&token, "span-validation", &body).await.0,
        StatusCode::CREATED
    );
    let before = h.projection(source, "span-validation").await;
    let rows = h.messages(source, "span-validation").await;
    assert_eq!(before["messages"][0], projected);
    body["base_revision"] = json!("1");
    for (field, searchable) in [
        ("conversation_spans", "conversation"),
        ("tool_spans", "tools"),
    ] {
        let mut missing = body.clone();
        missing["retrieval"]["messages"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert_eq!(
            h.post(&token, "span-validation", &missing).await.0,
            StatusCode::BAD_REQUEST
        );
        for malformed in [
            Value::Null,
            json!([[0, 1, 2]]),
            json!([[-1, 1]]),
            json!([[0, 1.5]]),
            json!(vec![[0, 0]; 100_001]),
        ] {
            let mut invalid = body.clone();
            invalid["retrieval"]["messages"][0][field] = malformed;
            assert_eq!(
                h.post(&token, "span-validation", &invalid).await.0,
                StatusCode::BAD_REQUEST
            );
        }
        for spans in [
            json!([[0, text.len() + 1]]),
            json!([[4, 0]]),
            json!([[1, 4]]),
            json!([[0, 1]]),
            json!([cafes[0], cafes[0]]),
            json!([cafes[1], cafes[0]]),
            json!([cafes[0]]),
            json!([]),
        ] {
            let mut invalid = body.clone();
            invalid["retrieval"]["messages"][0][field] = spans;
            invalid["retrieval"]["messages"][0][searchable] = json!("café\ncafé");
            assert_eq!(
                h.post(&token, "span-validation", &invalid).await,
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    json!({"error": {"code": "invalid_manifest"}})
                )
            );
        }
    }
    for (field, value) in [
        ("conversation", "cafécafé"),
        ("conversation", "café\ncaféX"),
        ("tools", "工具X"),
        ("tools", ""),
    ] {
        let mut invalid = body.clone();
        invalid["retrieval"]["messages"][0][field] = json!(value);
        assert_eq!(
            h.post(&token, "span-validation", &invalid).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(h.projection(source, "span-validation").await, before);
    assert_eq!(h.messages(source, "span-validation").await, rows);
    assert_eq!(h.inventory(&token).await["sessions"][0]["revision"], "1");

    // Only the spans change: identical selected text can occur more than once.
    body["retrieval"]["messages"][0]["conversation_spans"] = json!([cafes[0], cafes[2]]);
    assert_eq!(
        h.post(&token, "span-validation", &body).await,
        (StatusCode::OK, json!({"revision": "2", "changed": false}))
    );
    let reindexed = h.messages(source, "span-validation").await;
    assert_eq!(reindexed[0].2, rows[0].2);
    assert_ne!(reindexed[0].3, rows[0].3);
    assert_eq!(
        h.projection(source, "span-validation").await["messages"][0]["conversation_spans"],
        json!([cafes[0], cafes[2]])
    );
    body["base_revision"] = json!("2");
    assert_eq!(
        h.post(&token, "span-validation", &body).await.1["changed"],
        false
    );
    assert_eq!(h.messages(source, "span-validation").await, reindexed);

    body["base_revision"] = json!("3");
    body["upsert_messages"] = json!([{"id": "m", "mutation": true}]);
    body.as_object_mut().unwrap().remove("retrieval");
    assert_eq!(
        h.post(&token, "span-validation", &body).await.0,
        StatusCode::OK
    );
    let invalidated = h.projection(source, "span-validation").await;
    assert_eq!(
        invalidated["messages"],
        json!([{"id": "m", "role": null, "text": null,
        "conversation": null, "tools": null, "conversation_spans": null, "tool_spans": null}])
    );
    assert!(invalidated["title"].is_null());
    body["base_revision"] = json!("4");
    body["retrieval"] = retrieval(json!([projected]));
    body["retrieval"]["messages"][0]["conversation_spans"] = json!([[1, 4]]);
    let legacy_rows = h.messages(source, "span-validation").await;
    assert_eq!(
        h.post(&token, "span-validation", &body).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(h.projection(source, "span-validation").await, invalidated);
    assert_eq!(h.messages(source, "span-validation").await, legacy_rows);

    let mut empty = snapshot("span-empty", None, &["m"], json!([{"id": "m"}]));
    empty["retrieval"] = retrieval(
        json!([{"id": "m", "role": "", "text": "", "conversation": "", "tools": "",
        "conversation_spans": [], "tool_spans": [[0, 0]]}]),
    );
    assert_eq!(
        h.post(&token, "span-empty", &empty).await.0,
        StatusCode::CREATED
    );

    let spans: Vec<_> = (0..100_000)
        .map(|index| [index * 2, index * 2 + 2])
        .collect();
    let searchable = vec!["é"; 100_000].join("\n");
    let mut maximum = snapshot("span-maximum", None, &["m"], json!([{"id": "m"}]));
    maximum["retrieval"] = retrieval(
        json!([{"id": "m", "role": "generic", "text": "é".repeat(100_000),
        "conversation": searchable, "tools": searchable, "conversation_spans": spans, "tool_spans": spans}]),
    );
    assert_eq!(
        h.post(&token, "span-maximum", &maximum).await.0,
        StatusCode::CREATED
    );
    let stored = h.projection(source, "span-maximum").await;
    assert_eq!(
        stored["messages"][0]["conversation_spans"],
        maximum["retrieval"]["messages"][0]["conversation_spans"]
    );
    assert_eq!(
        stored["messages"][0]["tool_spans"],
        maximum["retrieval"]["messages"][0]["tool_spans"]
    );
}

async fn large_sessions(h: &Harness) {
    let (source, token) = h.source("large session");
    let order: Vec<_> = (0..100_000).map(|index| format!("m{index}")).collect();
    let ids: Vec<_> = order.iter().map(String::as_str).collect();
    let mut body = snapshot(
        "large",
        None,
        &ids,
        Value::Array(order.iter().map(|id| json!({"id": id})).collect()),
    );
    assert_eq!(h.post(&token, "large", &body).await.0, StatusCode::CREATED);
    body["upsert_messages"] = json!([]);
    let first = h.messages(source, "large").await;
    for revision in 1..=6 {
        body["base_revision"] = json!(revision.to_string());
        assert_eq!(
            h.post(&token, "large", &body).await,
            (
                StatusCode::OK,
                json!({"revision": (revision + 1).to_string(), "changed": false})
            )
        );
    }
    assert_eq!(h.messages(source, "large").await, first);
    body["base_revision"] = json!("7");
    body["message_order"].as_array_mut().unwrap().reverse();
    assert_eq!(h.post(&token, "large", &body).await.0, StatusCode::OK);
    let reversed = h.messages(source, "large").await;
    assert_eq!(
        reversed
            .iter()
            .map(|row| (&row.0, row.1))
            .collect::<Vec<_>>(),
        order
            .iter()
            .rev()
            .enumerate()
            .map(|(position, id)| (id, position as i32))
            .collect::<Vec<_>>()
    );
    body["base_revision"] = json!("8");
    body["message_order"].as_array_mut().unwrap().truncate(3);
    assert_eq!(h.post(&token, "large", &body).await.0, StatusCode::OK);
    let rows = h.messages(source, "large").await;
    assert_eq!(
        rows.iter()
            .map(|row| (row.0.as_str(), row.1))
            .collect::<Vec<_>>(),
        [("m99999", 0), ("m99998", 1), ("m99997", 2)]
    );
}

async fn credentials_and_purge(h: &Harness) {
    let (source, token) = h.source("admin");
    let body = snapshot(
        "purge",
        None,
        &["secret"],
        json!([{"id": "secret", "text": "disposable"}]),
    );
    assert_eq!(h.post(&token, "purge", &body).await.0, StatusCode::CREATED);
    assert_eq!(
        h.client
            .delete(format!("{}/v1/sync/sessions/purge", h.url))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.client
            .post(format!("{}/v1/sync/sessions/purge/purge", h.url))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let purged = h.cli(&["session", "purge", &source.to_string(), "purge"]);
    assert_eq!(purged["revision"], "2");
    assert!(h.messages(source, "purge").await.is_empty());
    let tombstone: (bool, bool) = sqlx::query_as("SELECT info IS NULL, info_hash IS NULL FROM sessions WHERE source_id = $1 AND session_id = 'purge'")
        .bind(source).fetch_one(&h.pool).await.unwrap();
    assert_eq!(tombstone, (true, true));
    assert_eq!(h.post(&token, "purge", &body).await.0, StatusCode::GONE);
    let update = snapshot("purge", Some("2"), &[], json!([]));
    assert_eq!(h.post(&token, "purge", &update).await.0, StatusCode::GONE);
    assert_eq!(
        h.inventory(&token).await["sessions"][0],
        json!({"session_id": "purge", "revision": "2", "deleted": true})
    );
    h.cli(&["session", "purge", &source.to_string(), "not-yet-created"]);
    assert_eq!(
        h.post(
            &token,
            "not-yet-created",
            &snapshot("not-yet-created", None, &[], json!([]))
        )
        .await
        .0,
        StatusCode::GONE
    );
    let racing = snapshot(
        "purge-race",
        None,
        &["m"],
        json!([{"id": "m", "text": "before"}]),
    );
    assert_eq!(
        h.post(&token, "purge-race", &racing).await.0,
        StatusCode::CREATED
    );
    let database_url = h.database_url.clone();
    let purge = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_spool-server"))
            .env("DATABASE_URL", database_url)
            .args(["session", "purge", &source.to_string(), "purge-race"])
            .output()
            .unwrap()
    });
    let update = snapshot(
        "purge-race",
        Some("1"),
        &["m"],
        json!([{"id": "m", "text": "racing update"}]),
    );
    let (uploaded, purged) = tokio::join!(h.post(&token, "purge-race", &update), purge);
    assert!(matches!(uploaded.0, StatusCode::OK | StatusCode::GONE));
    assert!(purged.unwrap().status.success());
    assert!(h.messages(source, "purge-race").await.is_empty());
    let erased: bool = sqlx::query_scalar("SELECT info IS NULL AND deleted_at IS NOT NULL FROM sessions WHERE source_id = $1 AND session_id = 'purge-race'")
        .bind(source).fetch_one(&h.pool).await.unwrap();
    assert!(
        erased,
        "purge must win in the final state regardless of upload ordering"
    );
    assert_eq!(
        h.post(&token, "purge-race", &racing).await.0,
        StatusCode::GONE
    );
    let rotated = h.cli(&["source", "rotate", &source.to_string()]);
    let new_token = rotated["token"].as_str().unwrap();
    assert_eq!(
        h.post(&token, "purge", &body).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.inventory(new_token).await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    h.cli(&["source", "revoke", &source.to_string()]);
    for denied in [new_token, "wrong", "spool_v1.invalid.invalid"] {
        assert_eq!(
            h.post(denied, "purge", &body).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    let response = h
        .client
        .get(format!("{}/v1/sync/sessions", h.url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        "unauthorized"
    );
    let last_seen: bool =
        sqlx::query_scalar("SELECT last_seen_at IS NOT NULL FROM sources WHERE id = $1")
            .bind(source)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    assert!(last_seen);
}

async fn admission_and_limits(h: &Harness) {
    let (_, token) = h.source("limits");
    let url = format!("{}/v1/sync/sessions/limits/snapshot", h.url);
    let invalid_path = h
        .client
        .post(format!("{}/v1/sync/sessions/%FF/snapshot", h.url))
        .bearer_auth(&token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid_path.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_path.json::<Value>().await.unwrap()["error"]["code"],
        "malformed_request"
    );
    for (content_type, encoding, expected) in [
        ("text/plain", "identity", "unsupported_content_type"),
        ("application/json", "gzip", "unsupported_content_encoding"),
    ] {
        let response = h
            .client
            .post(&url)
            .bearer_auth(&token)
            .header("content-type", content_type)
            .header("content-encoding", encoding)
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            expected
        );
    }
    let malformed = h
        .client
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body("{")
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    let oversized = h
        .client
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(vec![b' '; 16 * 1024 * 1024 + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let mut chunked = tokio::net::TcpStream::connect(h.url.strip_prefix("http://").unwrap())
        .await
        .unwrap();
    chunked.write_all(format!("POST /v1/sync/sessions/limits/snapshot HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let chunk = vec![b' '; 1024 * 1024];
    for _ in 0..16 {
        chunked.write_all(b"100000\r\n").await.unwrap();
        chunked.write_all(&chunk).await.unwrap();
        chunked.write_all(b"\r\n").await.unwrap();
    }
    chunked.write_all(b"1\r\nx\r\n0\r\n\r\n").await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        chunked.read_to_string(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(response.starts_with("HTTP/1.1 413"));
    assert!(response.contains("payload_too_large"));
    let mut large = snapshot(
        "limits",
        None,
        &["large"],
        json!([{"id": "large", "text": ""}]),
    );
    let empty_length = serde_json::to_vec(&large).unwrap().len();
    large["upsert_messages"][0]["text"] = json!("x".repeat(16 * 1024 * 1024 - empty_length));
    assert_eq!(
        h.post(&token, "limits", &large).await.0,
        StatusCode::CREATED
    );
    let mut held = Vec::new();
    for _ in 0..2 {
        let address = h.url.strip_prefix("http://").unwrap();
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream.write_all(format!("POST /v1/sync/sessions/limits/snapshot HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: 1000\r\nExpect: 100-continue\r\n\r\n").as_bytes()).await.unwrap();
        let mut response = [0; 25];
        tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&response, b"HTTP/1.1 100 Continue\r\n\r\n");
        held.push(stream);
    }
    let response = h
        .client
        .post(&url)
        .bearer_auth(&token)
        .json(&snapshot("limits", None, &[], json!([])))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"]["code"],
        "busy"
    );
    drop(held);
}

async fn retrieval_limits(h: &Harness) {
    const ARCHIVE: usize = 16 * 1024 * 1024;
    const ENVELOPE: usize = 64 * 1024 * 1024;
    let (_, token) = h.source("retrieval limits");
    let url = format!("{}/v1/sync/sessions/retrieval-limits/snapshot", h.url);
    let mut body = snapshot("retrieval-limits", None, &["m"], json!([{"id": "m"}]));
    // Unknown archive fields also count, without interpreting or normalizing them.
    body["padding"] = json!("");
    let overhead = serde_json::to_vec(&body).unwrap().len();
    body["padding"] = json!("x".repeat(ARCHIVE - overhead));
    assert_eq!(serde_json::to_vec(&body).unwrap().len(), ARCHIVE);
    body["retrieval"] = retrieval(json!([projection("m", "small")]));
    assert_eq!(
        h.post(&token, "retrieval-limits", &body).await.0,
        StatusCode::CREATED
    );

    body["base_revision"] = json!("1");
    // Changing null to "1" saves one byte; restore the original archive boundary.
    body["padding"] = json!(format!("{}x", body["padding"].as_str().unwrap()));
    let mut archive = body.clone();
    archive.as_object_mut().unwrap().remove("retrieval");
    let archive = serde_json::to_string(&archive).unwrap();
    assert_eq!(archive.len(), ARCHIVE);
    let projected = serde_json::to_string(&body["retrieval"]).unwrap();
    let first = format!("{{  \"retrieval\" : {projected} \n,{}", &archive[1..]);
    let response = h
        .client
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(first)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    body["base_revision"] = json!("2");
    let archive = archive.replacen("\"base_revision\":\"1\"", "\"base_revision\":\"2\"", 1);
    let last = format!(
        "{}, \"\\u0072etrieval\" \n: {projected}}}",
        &archive[..archive.len() - 1]
    );
    let response = h
        .client
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(last)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    body["base_revision"] = json!("3");
    body["padding"] = json!(format!("{}x", body["padding"].as_str().unwrap()));
    assert_eq!(
        h.post(&token, "retrieval-limits", &body).await,
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            json!({"error": {"code": "payload_too_large"}})
        )
    );
    body["padding"] = json!("x".repeat(ARCHIVE - overhead + 1));
    let overhead = serde_json::to_vec(&body).unwrap().len();
    body["retrieval"]["messages"][0]["text"] = json!(format!(
        "{}{}",
        body["retrieval"]["messages"][0]["text"].as_str().unwrap(),
        "x".repeat(ENVELOPE - overhead)
    ));
    assert_eq!(serde_json::to_vec(&body).unwrap().len(), ENVELOPE);
    assert_eq!(
        h.post(&token, "retrieval-limits", &body).await.0,
        StatusCode::OK
    );

    let response = h
        .client
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(vec![b' '; ENVELOPE + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let mut count = snapshot("retrieval-limits", Some("4"), &[], json!([]));
    count["retrieval"] = retrieval(json!(vec![projection("m", ""); 100_001]));
    assert_eq!(
        h.post(&token, "retrieval-limits", &count).await.0,
        StatusCode::BAD_REQUEST
    );
}
