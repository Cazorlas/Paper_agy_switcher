use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Bytes,
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};

use super::MockResponse;

pub const LOAD: &str = "/v1internal:loadCodeAssist";
pub const MODELS: &str = "/v1internal:fetchAvailableModels";
pub const SUMMARY: &str = "/v1internal:retrieveUserQuotaSummary";

#[derive(Debug, Clone)]
pub struct Call {
    pub path: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

#[derive(Default)]
struct ServerState {
    replies: HashMap<String, VecDeque<MockResponse>>,
    calls: Vec<Call>,
}

pub struct AgyServer {
    pub base: String,
    state: Arc<Mutex<ServerState>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl AgyServer {
    // Each route advances through its script, then repeats its last response.
    pub async fn start(scripts: Vec<(&str, Vec<MockResponse>)>) -> Self {
        let state = Arc::new(Mutex::new(ServerState {
            replies: scripts.into_iter().map(|(path, replies)| (path.into(), replies.into())).collect(),
            calls: Vec::new(),
        }));
        let app = Router::new()
            .route("/token", post(handler))
            .route(LOAD, post(handler))
            .route(MODELS, post(handler))
            .route(SUMMARY, post(handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async { let _ = stopped.await; })
                .await
                .unwrap();
        });
        Self { base, state, shutdown: Some(shutdown) }
    }

    pub fn calls(&self) -> Vec<Call> {
        self.state.lock().unwrap().calls.clone()
    }
}

impl Drop for AgyServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

async fn handler(
    State(state): State<Arc<Mutex<ServerState>>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut state = state.lock().unwrap();
    state.calls.push(Call { path: uri.path().into(), headers, body: body.to_vec() });
    let Some(script) = state.replies.get_mut(uri.path()) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "unscripted route").into_response();
    };
    let reply = if script.len() > 1 { script.pop_front() } else { script.front().cloned() };
    match reply {
        Some(MockResponse::Json(status, body)) => (status, axum::Json(body)).into_response(),
        Some(MockResponse::Text(status, body)) => (status, body).into_response(),
        None => (StatusCode::INTERNAL_SERVER_ERROR, "empty script").into_response(),
    }
}
