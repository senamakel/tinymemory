//! LivingBrain client tests against a local simulation of the public API.

#![allow(clippy::expect_used)]

use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use serde_json::{json, Value};

use super::{Capture, CaptureKind, ChatSender, ChatTurn, LivingBrain};

#[derive(Default)]
struct ApiState {
    captured: Mutex<Vec<Value>>,
}

fn authorized(headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .is_some_and(|value| value == "Bearer test-key")
        && headers
            .get("x-subject-id")
            .is_some_and(|value| value == "subject-1")
}

async fn capture(
    State(state): State<Arc<ApiState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    if !authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "missing auth"})),
        );
    }
    state.captured.lock().expect("state lock").push(body);
    (
        StatusCode::CREATED,
        Json(json!({"id": "source-1", "status": "queued"})),
    )
}

async fn search(headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    if !authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "missing auth"})),
        );
    }
    assert_eq!(body["query"], "customer preferences");
    assert_eq!(body["topK"], 3);
    (
        StatusCode::OK,
        Json(json!([{
            "pageId": "page-1",
            "slug": "customer-preferences",
            "title": "Customer preferences",
            "summary": "Prefers short weekly updates.",
            "pageType": "entity",
            "status": "active",
            "similarity": 0.92
        }])),
    )
}

async fn chat_turn(headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    if !authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "missing auth"})),
        );
    }
    assert_eq!(body["sender"], "user");
    (
        StatusCode::CREATED,
        Json(json!({"worthy": true, "reason": "durable preference", "sourceId": "source-2"})),
    )
}

async fn page(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if !authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "missing auth"})),
        );
    }
    (
        StatusCode::OK,
        Json(json!({"slug": "customer-preferences", "content": "..."})),
    )
}

async fn graph(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if !authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "missing auth"})),
        );
    }
    (
        StatusCode::OK,
        Json(json!({"nodes": [{"id": "page-1"}], "edges": []})),
    )
}

async fn sources(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if !authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "missing auth"})),
        );
    }
    (
        StatusCode::OK,
        Json(json!({"items": [{"id": "source-1", "status": "ready"}]})),
    )
}

async fn export(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if !authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "missing auth"})),
        );
    }
    (
        StatusCode::OK,
        Json(json!({"brainId": "brain-1", "markdown": "# Customer"})),
    )
}

async fn remove_source(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if !authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "missing auth"})),
        );
    }
    (StatusCode::OK, Json(json!({"deleted": true})))
}

async fn simulated_client() -> (LivingBrain, Arc<ApiState>) {
    let state = Arc::new(ApiState::default());
    let app = Router::new()
        .route("/v1/brains/brain-1/captures", post(capture))
        .route("/v1/brains/brain-1/captures/chat-turn", post(chat_turn))
        .route("/v1/brains/brain-1/search", post(search))
        .route("/v1/brains/brain-1/pages/customer-preferences", get(page))
        .route("/v1/brains/brain-1/graph", get(graph))
        .route("/v1/brains/brain-1/sources", get(sources))
        .route("/v1/brains/brain-1/export/markdown", get(export))
        .route("/v1/brains/brain-1/sources/source-1", delete(remove_source))
        .with_state(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind simulated API");
    let endpoint = format!("http://{}", listener.local_addr().expect("address"));
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve simulated API");
    });
    (
        LivingBrain::new(&endpoint, "test-key", "subject-1", "brain-1").expect("client"),
        state,
    )
}

#[tokio::test]
async fn simulated_api_carries_required_headers_and_native_payloads() {
    let (client, state) = simulated_client().await;
    let receipt = client
        .capture(&Capture {
            kind: CaptureKind::Note,
            content: Some("Weekly updates should be concise".into()),
            fetch_url: None,
            origin_ref: Some("event:123".into()),
            label: Some("Call notes".into()),
        })
        .await
        .expect("capture");
    assert_eq!(receipt.id, "source-1");
    assert_eq!(receipt.status.as_deref(), Some("queued"));
    assert_eq!(
        state.captured.lock().expect("state lock")[0]["originRef"],
        "event:123"
    );

    let results = client
        .search("customer preferences", 3, Some(0.5))
        .await
        .expect("search");
    assert_eq!(results[0].slug, "customer-preferences");
    assert_eq!(
        client.page("customer-preferences").await.expect("page")["content"],
        "..."
    );
    assert_eq!(
        client.graph().await.expect("graph")["nodes"][0]["id"],
        "page-1"
    );
    assert_eq!(
        client.sources().await.expect("sources")[0]
            .status
            .as_deref(),
        Some("ready")
    );
    let chat = client
        .capture_chat_turn(&ChatTurn {
            text: "I prefer concise weekly updates.".into(),
            sender: ChatSender::User,
            origin_ref: Some("chat:123".into()),
            agent_name: None,
        })
        .await
        .expect("chat turn");
    assert!(chat.worthy);
    assert_eq!(chat.source_id.as_deref(), Some("source-2"));
    client.remove_source("source-1").await.expect("cleanup");
    assert_eq!(
        client.export_markdown().await.expect("export").markdown,
        "# Customer"
    );
}

#[test]
fn connection_fields_and_capture_shape_are_checked_without_a_request() {
    let error = LivingBrain::cloud("", "subject", "brain").expect_err("blank key");
    assert!(format!("{error}").contains("API key"));
    let client = LivingBrain::cloud("test-key", "subject", "brain").expect("client");
    let rendered = format!("{client:?}");
    assert!(
        !rendered.contains("test-key"),
        "credential leaked: {rendered}"
    );
    let invalid = Capture {
        kind: CaptureKind::Note,
        content: Some("text".into()),
        fetch_url: Some("https://example.test".into()),
        origin_ref: None,
        label: None,
    };
    assert!(invalid.validate().is_err());
}
