use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use spool_server::{admin, browse_router, connect, connect_browse, migrate, router};
use sqlx::{Executor, PgPool};
use uuid::Uuid;

struct Harness {
    pool: PgPool,
    client: Client,
    reader: String,
    ingestion: String,
}

async fn serve(router: axum::Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, task)
}

fn projection(id: &str, text: &str, conversation: &str, tools: &str) -> Value {
    let span = |value: &str| -> Vec<[usize; 2]> {
        if value.is_empty() {
            Vec::new()
        } else {
            let start = text.find(value).unwrap();
            vec![[start, start + value.len()]]
        }
    };
    json!({
        "id": id, "role": "assistant", "text": text, "conversation": conversation, "tools": tools,
        "conversation_spans": span(conversation), "tool_spans": span(tools),
    })
}

impl Harness {
    async fn upload(
        &self,
        token: &str,
        id: &str,
        repository: Option<&str>,
        title: &str,
        projections: Vec<Value>,
    ) {
        let messages: Vec<_> = projections
            .iter()
            .map(|item| json!({"id": item["id"], "opaque": "original\u{0}export"}))
            .collect();
        let body = json!({
            "base_revision": null, "format": "opencode.v2", "upstream_version": "fixture", "collector_version": "fixture",
            "captured_at": "2026-11-09T02:00:00Z", "info": {"id": id},
            "message_order": messages.iter().map(|item| item["id"].clone()).collect::<Vec<_>>(),
            "upsert_messages": messages,
            "retrieval": {"version": 1, "repository": repository, "title": title, "activity": 1000, "messages": projections},
        });
        let response = self
            .client
            .post(format!("{}/v1/sync/sessions/{id}/snapshot", self.ingestion))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = response.text().await.unwrap();
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    async fn call(&self, operation: &str, input: Value) -> Value {
        let response = self
            .client
            .post(format!("{}/api/agent/{operation}", self.reader))
            .json(&input)
            .send()
            .await
            .unwrap();
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
        let status = response.status();
        let bytes = response.bytes().await.unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        assert!(
            bytes.len() <= 24 * 1024,
            "oversized {operation}: {}",
            bytes.len()
        );
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn error(&self, operation: &str, input: Value, status: StatusCode, code: &str) -> Value {
        let response = self
            .client
            .post(format!("{}/api/agent/{operation}", self.reader))
            .json(&input)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["error"]["code"], code, "{body}");
        body
    }

    async fn read_all(&self, input: Value) -> (BTreeMap<i64, String>, usize) {
        let mut page = self.call("read", input).await;
        let mut messages = BTreeMap::<i64, String>::new();
        let mut pages = 0;
        loop {
            pages += 1;
            assert!(pages < 1000, "continuation did not progress");
            assert!(!page["messages"].as_array().unwrap().is_empty());
            for message in page["messages"].as_array().unwrap() {
                let text = messages
                    .entry(message["index"].as_i64().unwrap())
                    .or_default();
                assert_eq!(message["offset_bytes"], text.len());
                text.push_str(message["text"].as_str().unwrap());
                assert!(text.len() as i64 <= message["total_bytes"].as_i64().unwrap());
            }
            if page["next_cursor"].is_null() {
                break;
            }
            page = self
                .call("read", json!({"cursor": page["next_cursor"]}))
                .await;
        }
        (messages, pages)
    }
}

struct Assets(PathBuf);

impl Drop for Assets {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[tokio::test]
async fn private_repository_search_and_bounded_evidence() {
    let Ok(database_url) = std::env::var("SPOOL_TEST_DATABASE_URL") else {
        eprintln!(
            "retrieval PostgreSQL regression skipped: set SPOOL_TEST_DATABASE_URL to a disposable superuser database"
        );
        return;
    };
    let owner = connect(&database_url).await.unwrap();
    let database = format!("spool_retrieval_{}", Uuid::new_v4().simple());
    let role = format!("spool_retrieval_reader_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&owner)
        .await
        .unwrap();
    let cleanup_database = database.clone();
    let cleanup_role = role.clone();
    let result = tokio::spawn(async move {
        let mut database_url = reqwest::Url::parse(&database_url).unwrap();
        database_url.set_path(&database);
        let pool = connect(database_url.as_str()).await.unwrap();
        migrate(&pool).await.unwrap();
        pool.execute(
            include_str!("../../deploy/reader-role.sql")
                .replace("spool_reader", &role)
                .as_str(),
        )
        .await
        .unwrap();
        pool.execute(
            format!("ALTER ROLE {role} LOGIN PASSWORD 'synthetic-retrieval-reader'").as_str(),
        )
        .await
        .unwrap();
        let a = admin::add_source(&pool, "synthetic laptop").await.unwrap();
        let b = admin::add_source(&pool, "synthetic desktop").await.unwrap();
        database_url.set_username(&role).unwrap();
        database_url
            .set_password(Some("synthetic-retrieval-reader"))
            .unwrap();
        let reader_pool = connect_browse(database_url.as_str()).await.unwrap();
        let assets =
            Assets(std::env::temp_dir().join(format!("spool-retrieval-{}", Uuid::new_v4())));
        std::fs::create_dir_all(&assets.0).unwrap();
        std::fs::write(
            assets.0.join("index.html"),
            "<!doctype html><title>Fixture</title>",
        )
        .unwrap();
        let (reader, reader_task) =
            serve(browse_router(reader_pool.clone(), &assets.0).unwrap()).await;
        let (ingestion, ingestion_task) = serve(router(pool.clone())).await;
        let harness = Harness {
            pool: pool.clone(),
            client: Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap(),
            reader,
            ingestion,
        };
        exercise(&harness, &a, &b).await;
        reader_task.abort();
        ingestion_task.abort();
        reader_pool.close().await;
        pool.close().await;
    })
    .await;
    let database_result = sqlx::query(&format!("DROP DATABASE {cleanup_database} WITH (FORCE)"))
        .execute(&owner)
        .await;
    let role_result = sqlx::query(&format!("DROP ROLE IF EXISTS {cleanup_role}"))
        .execute(&owner)
        .await;
    owner.close().await;
    database_result.unwrap();
    role_result.unwrap();
    result.unwrap();
}

async fn exercise(h: &Harness, a: &admin::Credential, b: &admin::Credential) {
    let repository = "example.test/team/atlas";
    let giant = format!(
        "{}needle pipeline final evidence{}",
        "東京🙂\n\"\\".repeat(8000),
        "后续".repeat(5000)
    );
    let old = "needle pipeline original decision, alpha 50%_done";
    let revised = "needle pipeline later correction: beta";
    h.upload(
        &a.token,
        "same",
        Some(repository),
        "Archive fixture",
        vec![
            projection("msg_z", &giant, &giant, ""),
            projection(
                "msg_a",
                &format!("{old}\nTool: shell; status: error\nsecret_tool_marker exit 7"),
                old,
                "secret_tool_marker exit 7",
            ),
            projection(
                "msg_m",
                "Unknown variant JSON: {\"literal\":\"50%_done\\u0000\"}",
                "",
                "",
            ),
            projection("msg_tail", revised, revised, ""),
        ],
    )
    .await;
    h.upload(
        &b.token,
        "same",
        Some(repository),
        "Independent copy",
        vec![projection(
            "msg_z",
            "needle pipeline second machine",
            "needle pipeline second machine",
            "",
        )],
    )
    .await;
    h.upload(
        &a.token,
        "other",
        Some("example.test/another/project"),
        "Other repository",
        vec![projection(
            "msg_o",
            "needle pipeline unrelated",
            "needle pipeline unrelated",
            "",
        )],
    )
    .await;
    h.upload(
        &b.token,
        "unassigned",
        None,
        "No repository",
        vec![projection(
            "msg_u",
            "unassigned content",
            "unassigned content",
            "",
        )],
    )
    .await;
    let legacy = json!({
        "base_revision": null, "format": "opencode.v2", "upstream_version": "legacy", "collector_version": "legacy",
        "captured_at": "2026-11-09T02:00:00Z", "info": {"id": "legacy"}, "message_order": [], "upsert_messages": [],
    });
    assert_eq!(
        h.client
            .post(format!("{}/v1/sync/sessions/legacy/snapshot", h.ingestion))
            .bearer_auth(&a.token)
            .json(&legacy)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CREATED
    );

    let found = h
        .call(
            "find",
            json!({"query": "NEEDLE pipeline", "repository": repository, "limit": 1}),
        )
        .await;
    assert_eq!(
        found["coverage"],
        json!({"total_sessions": 5, "indexed_sessions": 4, "unassigned_repository_sessions": 2})
    );
    assert_eq!(found["sessions"].as_array().unwrap().len(), 1);
    let next = h
        .call("find", json!({"cursor": found["next_cursor"]}))
        .await;
    assert_eq!(next["sessions"].as_array().unwrap().len(), 1);
    let references: std::collections::BTreeSet<_> = [
        found["sessions"][0]["session"].as_str().unwrap(),
        next["sessions"][0]["session"].as_str().unwrap(),
    ]
    .into_iter()
    .collect();
    let ref_a = format!("{}/same", a.source_id);
    let ref_b = format!("{}/same", b.source_id);
    assert_eq!(
        references,
        [ref_a.as_str(), ref_b.as_str()].into_iter().collect()
    );
    assert!(next["next_cursor"].is_null());
    assert_eq!(
        h.call("find", json!({"query":"needle pipeline"})).await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        h.call("find", json!({"query":"archive fixture"})).await["sessions"][0]["match"],
        Value::Null
    );

    let mut page = h
        .call(
            "search",
            json!({"session": ref_a, "query": "needle PIPELINE", "limit": 1}),
        )
        .await;
    let stale_search = page["next_cursor"].clone();
    let mut indices = Vec::new();
    loop {
        indices.push(page["matches"][0]["index"].as_i64().unwrap());
        assert!(
            page["matches"][0]["excerpt"]
                .as_str()
                .unwrap()
                .chars()
                .count()
                <= 300
        );
        if page["next_cursor"].is_null() {
            break;
        }
        page = h
            .call("search", json!({"cursor": page["next_cursor"]}))
            .await;
    }
    assert_eq!(indices, vec![4, 2, 1]);
    let jump = h
        .call("read", json!({"cursor": page["matches"][0]["read_cursor"]}))
        .await;
    assert!(jump["messages"][0]["offset_bytes"].as_i64().unwrap() > 50_000);
    assert!(
        jump["messages"][0]["text"]
            .as_str()
            .unwrap()
            .contains("needle pipeline final evidence")
    );
    assert_eq!(
        h.call("search", json!({"session": ref_a, "query":"alpha beta"}))
            .await["matches"],
        json!([]),
        "terms must occur within one message"
    );
    assert_eq!(
        h.call(
            "search",
            json!({"session": ref_a, "query":"secret_tool_marker"})
        )
        .await["matches"],
        json!([])
    );
    let tools = h
        .call(
            "search",
            json!({"session": ref_a, "query":"secret_tool_marker", "include_tools": true}),
        )
        .await;
    assert_eq!(tools["matches"][0]["index"], 2);
    assert_eq!(
        h.call("find", json!({"query":"secret_tool_marker"})).await["sessions"],
        json!([])
    );
    assert_eq!(
        h.call(
            "find",
            json!({"query":"secret_tool_marker", "include_tools":true})
        )
        .await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        h.call("search", json!({"session": ref_a, "query":"50%_done"}))
            .await["matches"][0]["index"],
        2
    );
    for query in [
        "needle%",
        "needle_pipeline",
        "needle OR missing",
        "needle' OR '1'='1",
    ] {
        assert_eq!(
            h.call("search", json!({"session": ref_a, "query":query}))
                .await["matches"],
            json!([])
        );
    }

    let first_read = h
        .call("read", json!({"session":ref_a, "start":1,"end":4}))
        .await;
    let stale_read = first_read["next_cursor"].clone();
    let (messages, pages) = h
        .read_all(json!({"session":ref_a, "start":1,"end":4}))
        .await;
    assert!(pages > 3);
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[&1], giant);
    assert!(messages[&2].contains("status: error"));
    assert!(messages[&3].contains("\\u0000"));
    assert_eq!(messages[&4], revised);
    assert_eq!(
        h.call("read", json!({"session":ref_a,"start":5})).await["messages"],
        json!([])
    );
    assert_eq!(
        h.call("read", json!({"session":ref_a,"start":2,"end":2}))
            .await["next_cursor"],
        Value::Null
    );
    assert_eq!(
        h.call("read", json!({"session":ref_b})).await["messages"][0]["text"],
        "needle pipeline second machine"
    );
    h.error(
        "read",
        json!({"session":format!("{}/legacy",a.source_id)}),
        StatusCode::CONFLICT,
        "not_indexed",
    )
    .await;
    for (operation, body) in [
        ("find", json!({"source":a.source_id})),
        ("find", json!({"directory":"atlas"})),
        ("find", json!({"limit":51})),
        ("find", json!({"query":" "})),
        ("find", json!({"query":"x".repeat(513)})),
        (
            "search",
            json!({"session":ref_a,"query":"needle","limit":101}),
        ),
        ("search", json!({"cursor":stale_search,"query":"changed"})),
        ("read", json!({"cursor":stale_read,"start":1})),
        ("read", json!({"session":ref_a,"start":0})),
        ("read", json!({"session":ref_a,"start":4,"end":1})),
        ("read", json!({"session":ref_a,"revision":"not-a-number"})),
        ("read", json!({"session":"unqualified"})),
        ("read", json!({"cursor":stale_search})),
    ] {
        h.error(
            operation,
            body,
            StatusCode::BAD_REQUEST,
            "malformed_request",
        )
        .await;
    }
    for header in [
        ("origin", "https://evil.invalid"),
        ("sec-fetch-site", "cross-site"),
    ] {
        let response = h
            .client
            .post(format!("{}/api/agent/find", h.reader))
            .header(header.0, header.1)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(
        h.client
            .post(format!("{}/api/agent/find", h.ingestion))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );

    sqlx::query("UPDATE sessions SET revision = 2 WHERE source_id = $1 AND session_id = 'same'")
        .bind(a.source_id)
        .execute(&h.pool)
        .await
        .unwrap();
    for (operation, input) in [
        ("read", json!({"cursor":stale_read})),
        ("search", json!({"cursor":stale_search})),
        ("read", json!({"session":ref_a,"revision":"1"})),
    ] {
        let error = h
            .error(operation, input, StatusCode::CONFLICT, "session_changed")
            .await;
        assert_eq!(error["error"]["current_revision"], "2");
    }

    // Long references, repository names and Unicode titles force byte-bound discovery pages.
    let long_repository = format!("example.test/team/{}", "r".repeat(900));
    for index in 0..18 {
        let id = format!("stress_{index:03}_{}", "s".repeat(490));
        h.upload(
            &a.token,
            &id,
            Some(&long_repository),
            &"箱".repeat(1000),
            vec![],
        )
        .await;
    }
    let mut page = h
        .call(
            "find",
            json!({"query":"stress", "repository":long_repository,"limit":50}),
        )
        .await;
    let mut seen = std::collections::BTreeSet::new();
    let mut pages = 0;
    loop {
        pages += 1;
        assert!(pages < 30);
        for item in page["sessions"].as_array().unwrap() {
            assert!(seen.insert(item["session"].as_str().unwrap().to_owned()));
        }
        if page["next_cursor"].is_null() {
            break;
        }
        page = h.call("find", json!({"cursor":page["next_cursor"]})).await;
    }
    assert!(pages > 1);
    assert_eq!(seen.len(), 18);

    let tiny: Vec<_> = (0..205)
        .map(|index| projection(&format!("tiny_{index}_{}", "m".repeat(450)), "", "", ""))
        .collect();
    h.upload(
        &a.token,
        "tiny",
        Some(repository),
        "Many tiny messages",
        tiny,
    )
    .await;
    let (messages, pages) = h
        .read_all(json!({"session":format!("{}/tiny",a.source_id),"start":1,"end":205}))
        .await;
    assert_eq!(messages.len(), 205);
    assert!(pages > 1);
    admin::purge_session(&h.pool, a.source_id, "same")
        .await
        .unwrap();
    assert_eq!(
        h.call(
            "find",
            json!({"query":"needle pipeline","repository":repository})
        )
        .await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    h.error(
        "read",
        json!({"session":ref_a}),
        StatusCode::NOT_FOUND,
        "not_found",
    )
    .await;

    let conversation = "anchor pipeline chosen implementation";
    let tool = "toolanchor recorded stderr";
    let rendered = format!(
        "reasoning: anchor discarded speculation; toolanchor guess\n{}\n{conversation}\n{tool}",
        "无关🙂".repeat(10_000)
    );
    h.upload(
        &a.token,
        "eligible-passages",
        Some(repository),
        "Searchable evidence spans",
        vec![projection("evidence", &rendered, conversation, tool)],
    )
    .await;
    let reference = format!("{}/eligible-passages", a.source_id);
    for (query, include_tools, expected) in [
        ("anchor pipeline", false, conversation),
        ("toolanchor", true, tool),
    ] {
        let search = h
            .call(
                "search",
                json!({"session":reference,"query":query,"include_tools":include_tools}),
            )
            .await;
        let found = h
            .call("find", json!({"query":query,"include_tools":include_tools}))
            .await;
        let found = found["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["session"] == reference)
            .unwrap();
        for passage in [&search["matches"][0], &found["match"]] {
            assert!(passage["excerpt"].as_str().unwrap().contains(expected));
            assert!(!passage["excerpt"].as_str().unwrap().contains("speculation"));
            let page = h
                .call("read", json!({"cursor":passage["read_cursor"]}))
                .await;
            assert!(page["messages"][0]["offset_bytes"].as_i64().unwrap() > 50_000);
            assert!(
                page["messages"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains(expected)
            );
        }
    }
}
