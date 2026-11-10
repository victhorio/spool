use std::{collections::BTreeMap, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use spool_server::{admin, connect, connect_read, migrate, router};
use sqlx::PgPool;
use uuid::Uuid;

/// The reader URL the plugin sends, so results link to it.
const READER: &str = "https://spool.example";

struct Harness {
    pool: PgPool,
    client: Client,
    url: String,
    token: String,
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
            .post(format!("{}/v1/sync/sessions/{id}/snapshot", self.url))
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
            .post(format!("{}/v1/agent/{operation}", self.url))
            .bearer_auth(&self.token)
            .header("spool-reader-url", READER)
            .json(&input)
            .send()
            .await
            .unwrap();
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
            .post(format!("{}/v1/agent/{operation}", self.url))
            .bearer_auth(&self.token)
            .header("spool-reader-url", READER)
            .json(&input)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        let bytes = response.bytes().await.unwrap();
        assert!(
            bytes.len() <= 24 * 1024,
            "oversized {operation} error: {}",
            bytes.len()
        );
        let body: Value = serde_json::from_slice(&bytes).unwrap();
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
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&owner)
        .await
        .unwrap();
    let cleanup_database = database.clone();
    let result = tokio::spawn(async move {
        let mut database_url = reqwest::Url::parse(&database_url).unwrap();
        database_url.set_path(&database);
        let pool = connect(database_url.as_str()).await.unwrap();
        migrate(&pool).await.unwrap();
        let mut a = admin::add_source(&pool, "synthetic laptop").await.unwrap();
        // A fixed fixture identity lets literal v2 cursors outlive changes to their encoder.
        let source = Uuid::from_u128(1);
        sqlx::query("UPDATE sources SET id = $1 WHERE id = $2")
            .bind(source)
            .bind(a.source_id)
            .execute(&pool)
            .await
            .unwrap();
        a.token = a
            .token
            .replacen(&a.source_id.to_string(), &source.to_string(), 1);
        a.source_id = source;
        let b = admin::add_source(&pool, "synthetic desktop").await.unwrap();
        let read = connect_read(database_url.as_str()).await.unwrap();
        let (url, task) = serve(router(pool.clone(), read.clone(), None).unwrap()).await;
        let harness = Harness {
            pool: pool.clone(),
            client: Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap(),
            url,
            token: b.token.clone(),
        };
        exercise(&harness, &a, &b).await;
        aliases_and_compact_cursors(&harness, &a, &b).await;
        task.abort();
        read.close().await;
        pool.close().await;
    })
    .await;
    let database_result = sqlx::query(&format!("DROP DATABASE {cleanup_database} WITH (FORCE)"))
        .execute(&owner)
        .await;
    owner.close().await;
    database_result.unwrap();
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
            .post(format!("{}/v1/sync/sessions/legacy/snapshot", h.url))
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
        ("read", json!({"session":"/unqualified"})),
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
    let forged = format!("{}A", &h.token[..h.token.len() - 1]);
    for token in [None, Some(forged.as_str())] {
        let mut request = h.client.post(format!("{}/v1/agent/find", h.url));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request.json(&json!({})).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"error":{"code":"unauthorized"}})
        );
    }
    sqlx::query("UPDATE sources SET revoked_at = now() WHERE id = $1")
        .bind(a.source_id)
        .execute(&h.pool)
        .await
        .unwrap();
    let response = h
        .client
        .post(format!("{}/v1/agent/find", h.url))
        .bearer_auth(&a.token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    sqlx::query("UPDATE sources SET revoked_at = NULL WHERE id = $1")
        .bind(a.source_id)
        .execute(&h.pool)
        .await
        .unwrap();
    for (path, status) in [
        ("/api/agent/find", StatusCode::NOT_FOUND),
        ("/v1/agent/unknown", StatusCode::NOT_FOUND),
    ] {
        let response = h
            .client
            .post(format!("{}{path}", h.url))
            .bearer_auth(&h.token)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{path}");
    }

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
            .find(|item| item["session"] == "eligible-passages")
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

async fn aliases_and_compact_cursors(h: &Harness, a: &admin::Credential, b: &admin::Credential) {
    for (token, alias) in [(&a.token, "work-m5"), (&b.token, "home-m4")] {
        let response = h
            .client
            .put(format!("{}/v1/sync/source", h.url))
            .bearer_auth(token)
            .json(&json!({"alias":alias}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let id = format!("ses_{:0<26}", "compact_unique");
    let rendered = format!(
        "compactneedle original work evidence {}",
        "東京🙂\n\"\\".repeat(5000)
    );
    h.upload(
        &a.token,
        &id,
        None,
        "Compact cursor fixture",
        vec![
            projection("compact_1", &rendered, &rendered, ""),
            projection(
                "compact_2",
                "compactneedle later correction",
                "compactneedle later correction",
                "",
            ),
            projection(
                "compact_3",
                "compactneedle final decision",
                "compactneedle final decision",
                "",
            ),
        ],
    )
    .await;
    let qualified = format!("work-m5/{id}");
    let legacy_reference = format!("{}/{id}", a.source_id);
    for reference in [&id, &qualified, &legacy_reference] {
        let read = h.call("read", json!({"session":reference,"start":2})).await;
        assert_eq!(read["session"], id, "unique IDs need no qualifier");
        assert_eq!(read["url"], format!("{READER}/sessions/{id}"));
        assert_eq!(
            read["messages"][0]["url"],
            format!("{READER}/sessions/{id}#2")
        );
        assert_eq!(
            read["messages"][0]["text"],
            "compactneedle later correction"
        );
    }
    let searched = h
        .call(
            "search",
            json!({"session":id,"query":"compactneedle","limit":1}),
        )
        .await;
    assert_eq!(searched["session"], id);
    assert_eq!(
        searched["matches"][0]["url"],
        format!("{READER}/sessions/{id}#3")
    );
    assert_eq!(searched["matches"][0]["index"], 3);
    let reading = h
        .call("read", json!({"session":id,"start":1,"end":3}))
        .await;
    let legacy = |value: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap());
    let old_search = legacy(json!({"kind":"Search","query":{
        "session":legacy_reference,"query":"compactneedle","revision":null,"include_tools":false,"limit":1
    },"revision":1,"before":2}));
    let old_read = legacy(
        json!({"kind":"Read","session":legacy_reference,"revision":1,"start":1,"end":3,"offset":0}),
    );
    let search_cursor = searched["next_cursor"].as_str().unwrap();
    let read_cursor = searched["matches"][0]["read_cursor"].as_str().unwrap();
    assert!(search_cursor.len() * 2 < old_search.len());
    assert!(read_cursor.len() * 2 < old_read.len());
    assert!(
        read_cursor.len() <= 80,
        "normal evidence jump should fit in 80 characters: {}",
        read_cursor.len()
    );
    assert_eq!(h.call("read", json!({"cursor":old_read})).await, reading);
    assert_eq!(
        h.call("search", json!({"cursor":old_search})).await,
        h.call("search", json!({"cursor":search_cursor})).await
    );
    let v2_search = "AgEQAAAAAAAAAAAAAAAAAAAAAR5zZXNfY29tcGFjdF91bmlxdWUwMDAwMDAwMDAwMDANY29tcGFjdG5lZWRsZQIAAQEE";
    assert_eq!(
        h.call("search", json!({"cursor":v2_search})).await,
        h.call("search", json!({"cursor":old_search})).await
    );
    let v2_read = "AgIQAAAAAAAAAAAAAAAAAAAAAR5zZXNfY29tcGFjdF91bmlxdWUwMDAwMDAwMDAwMDACBAYA";
    assert_eq!(
        h.call("read", json!({"cursor":v2_read})).await,
        h.call("read", json!({"session":qualified,"start":2,"end":3}))
            .await
    );

    // Native IDs can become ambiguous later; issued cursors must stay bound to the original store.
    h.upload(
        &b.token,
        &id,
        None,
        "Independent compact copy",
        vec![projection(
            "copy",
            "compactneedle other machine only",
            "compactneedle other machine only",
            "",
        )],
    )
    .await;
    let expected_candidates: std::collections::BTreeSet<_> =
        [qualified.clone(), format!("home-m4/{id}")]
            .into_iter()
            .collect();
    for (operation, input) in [
        ("read", json!({"session":id})),
        (
            "search",
            json!({"session":id,"query":"compactneedle","revision":"1"}),
        ),
    ] {
        let error = h
            .error(operation, input, StatusCode::CONFLICT, "ambiguous_session")
            .await;
        let candidates = error["error"]["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(expected_candidates, candidates);
        assert_eq!(error["error"]["more"], false);
    }
    assert_eq!(
        h.call("search", json!({"cursor":search_cursor})).await["matches"][0]["index"],
        2
    );
    assert_eq!(
        h.call("read", json!({"cursor":read_cursor})).await["messages"][0]["text"],
        "compactneedle final decision"
    );
    let continuation = h
        .call("read", json!({"cursor":reading["next_cursor"]}))
        .await;
    assert_eq!(continuation["session"], qualified);
    assert_eq!(
        continuation["url"],
        format!("{READER}/sessions/{qualified}")
    );
    assert_eq!(
        continuation["messages"][0]["offset_bytes"],
        reading["messages"][0]["text"].as_str().unwrap().len()
    );

    let found = h
        .call("find", json!({"query":"compactneedle","limit":1}))
        .await;
    let v2_find = "AgABDWNvbXBhY3RuZWVkbGUAAAEBAtIPEAAAAAAAAAAAAAAAAAAAAAEec2VzX2NvbXBhY3RfdW5pcXVlMDAwMDAwMDAwMDAw";
    assert_eq!(h.call("find", json!({"cursor":v2_find})).await, found);
    let first = &found["sessions"][0];
    let first_reference = first["session"].as_str().unwrap();
    assert_eq!(first["url"], format!("{READER}/sessions/{first_reference}"));
    assert_eq!(
        first["match"]["url"],
        format!(
            "{READER}/sessions/{first_reference}#{}",
            first["match"]["index"]
        )
    );
    let first_source = if first_reference.starts_with("work-m5/") {
        a.source_id
    } else {
        b.source_id
    };
    let old_find = legacy(
        json!({"kind":"Find","query":{"query":"compactneedle","repository":null,"include_tools":false,"limit":1},
        "after":{"score":1,"activity":1000,"source":first_source,"session":id}}),
    );
    let find_cursor = found["next_cursor"].as_str().unwrap();
    assert!(find_cursor.len() * 2 < old_find.len());
    assert_eq!(
        h.call("find", json!({"cursor":find_cursor})).await,
        h.call("find", json!({"cursor":old_find})).await
    );
    eprintln!(
        "cursor characters: find {} -> {}, search {} -> {}, evidence read {} -> {}",
        old_find.len(),
        find_cursor.len(),
        old_search.len(),
        search_cursor.len(),
        old_read.len(),
        read_cursor.len()
    );
    for operation in ["find", "search", "read"] {
        let mut bytes = URL_SAFE_NO_PAD
            .decode(match operation {
                "find" => find_cursor,
                "search" => search_cursor,
                _ => read_cursor,
            })
            .unwrap();
        let mut unknown_version = bytes.clone();
        unknown_version[0] = 3;
        let mut trailing = bytes.clone();
        trailing.push(0);
        bytes.pop();
        for bytes in [bytes, unknown_version, trailing] {
            h.error(
                operation,
                json!({"cursor":URL_SAFE_NO_PAD.encode(bytes)}),
                StatusCode::BAD_REQUEST,
                "malformed_request",
            )
            .await;
        }
    }
    for reader in [
        "ftp://spool.example",
        "https://spool.example/",
        "spool.example",
    ] {
        let response = h
            .client
            .post(format!("{}/v1/agent/read", h.url))
            .bearer_auth(&h.token)
            .header("spool-reader-url", reader)
            .json(&json!({"session":qualified}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{reader}");
    }
    h.error(
        "read",
        json!({"session":"ses_missing_compact"}),
        StatusCode::NOT_FOUND,
        "not_found",
    )
    .await;
    h.error(
        "read",
        json!({"session":format!("unknown/{id}")}),
        StatusCode::NOT_FOUND,
        "not_found",
    )
    .await;
    for reference in [
        format!("bad_alias/{id}"),
        format!("{}/{id}", "x".repeat(33)),
        "work-m5/".to_owned(),
    ] {
        h.error(
            "read",
            json!({"session":reference}),
            StatusCode::BAD_REQUEST,
            "malformed_request",
        )
        .await;
    }

    // A 32-character hexadecimal alias is a name, not an unhyphenated UUID.
    let c = admin::add_source(&h.pool, "unaliased then named")
        .await
        .unwrap();
    h.upload(
        &c.token,
        "alias-edge",
        None,
        "Alias boundary",
        vec![projection(
            "c",
            "unaltered evidence",
            "unaltered evidence",
            "",
        )],
    )
    .await;
    assert_eq!(
        h.call(
            "read",
            json!({"session":format!("{}/alias-edge", c.source_id)})
        )
        .await["session"],
        "alias-edge"
    );
    let alias = "a".repeat(32);
    assert_eq!(
        h.client
            .put(format!("{}/v1/sync/source", h.url))
            .bearer_auth(&c.token)
            .json(&json!({"alias":alias}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        h.call("read", json!({"session":format!("{alias}/alias-edge")}))
            .await["messages"][0]["text"],
        "unaltered evidence"
    );
    let escaped = "\u{1}".repeat(512);
    h.upload(&a.token, &escaped, None, "Escaped native ID", vec![])
        .await;
    for index in 0..9 {
        let copy = admin::add_source(&h.pool, &format!("ambiguous {index}"))
            .await
            .unwrap();
        h.upload(&copy.token, &id, None, "More independent copies", vec![])
            .await;
        h.upload(&copy.token, &escaped, None, "Escaped native ID", vec![])
            .await;
    }
    let error = h
        .error(
            "read",
            json!({"session":id}),
            StatusCode::CONFLICT,
            "ambiguous_session",
        )
        .await;
    assert_eq!(error["error"]["candidates"].as_array().unwrap().len(), 10);
    assert_eq!(error["error"]["more"], true);
    for (operation, input) in [
        ("read", json!({"session":escaped})),
        ("search", json!({"session":escaped,"query":"any"})),
    ] {
        let error = h
            .error(operation, input, StatusCode::CONFLICT, "ambiguous_session")
            .await;
        let candidates = error["error"]["candidates"].as_array().unwrap();
        assert!((1..10).contains(&candidates.len()));
        assert!(
            candidates
                .iter()
                .all(|value| value.as_str().unwrap().ends_with(&escaped))
        );
        assert_eq!(error["error"]["more"], true);
        let read = h.call("read", json!({"session":candidates[0]})).await;
        assert!(
            read["url"].as_str().unwrap().ends_with(&"%01".repeat(512)),
            "references are percent-encoded in links"
        );
    }
}
