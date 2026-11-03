//! The gateway server.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::{Mutex, mpsc};

use crate::agent::session::{Session, SessionStore};
use crate::agent::{Agent, AgentEvent, SessionRunner};
use crate::config::Config;
use crate::gateway::protocol::{
    ClientMessage, ErrorResponse, EventFrame, GatewayStatus, HealthResponse, OutFrame,
    RequestParams, ResponseFrame, SendMessageRequest, SendMessageResponse, ServerMessage,
    SessionSummary, SystemStatus, WireEvent, split_event,
};
use crate::gateway::web;
use crate::inbox::{
    Filter as InboxFilter, InboxError, InboxStore, Kind as InboxKind, NewRecord, Owner,
    Priority as InboxPriority, Provenance, Record as InboxRecord, State as InboxRecordState,
    Update as InboxUpdate,
};
use crate::tool::approval::{ApprovalDecision, ApprovalRequest};

/// Everything the server needs.
#[derive(Clone)]
pub struct GatewayState {
    pub agent: Arc<Agent>,
    pub runner: Arc<SessionRunner>,
    pub sessions: Arc<Mutex<SessionStore>>,
    /// The local user's personal inbox, shared with the local CLI.
    pub inbox: Arc<Mutex<InboxStore>>,
    pub config: Arc<Config>,
    /// The voice, built once for the gateway so in-process Phonon is shared.
    pub voice: Option<Arc<crate::voice::Voice>>,
    /// How many skills were found when the gateway started.
    pub skills: usize,
}

/// A running gateway.
pub struct Gateway {
    state: GatewayState,
    addr: SocketAddr,
    /// Awaits the server's shutdown.
    handle: tokio::task::JoinHandle<()>,
}

impl Gateway {
    /// Bind and start the gateway.
    pub async fn start(
        agent: Arc<Agent>,
        sessions: Arc<Mutex<SessionStore>>,
        config: Arc<Config>,
    ) -> anyhow::Result<Self> {
        let host = config.gateway.host.clone();
        let port = config.gateway.port;
        let token = config.gateway.token.clone();

        let runner = SessionRunner::new(agent.clone(), sessions.clone());
        let inbox = Arc::new(Mutex::new(InboxStore::open(
            config.state_dir.join("inbox"),
        )?));

        let skills = config.discover_skills().len();
        let voice = if config.voice.enabled {
            let v = crate::voice::Voice::new(config.voice.clone());
            if v.can_speak() || v.can_synthesize() {
                Some(Arc::new(v))
            } else {
                None
            }
        } else {
            None
        };
        let state = GatewayState {
            agent,
            runner,
            sessions,
            inbox,
            config: config.clone(),
            voice,
            skills,
        };

        let app = Router::new()
            .route("/", get(web::index))
            .route("/assets/{*path}", get(web::asset_file))
            .route("/health", get(health))
            .route("/v1/system", get(system_status))
            .route("/v1/models", get(list_models))
            .route("/v1/tools", get(list_tools))
            .route("/v1/skills", get(list_skills))
            .route("/v1/config", get(get_config).patch(update_config))
            .route("/v1/sessions", get(list_sessions).post(create_session))
            .route("/v1/sessions/{id}", get(get_session).delete(delete_session))
            .route("/v1/sessions/{id}/messages", post(send_message))
            .route("/v1/inbox", get(list_inbox).post(capture_inbox))
            .route(
                "/v1/inbox/{id}",
                get(get_inbox_record)
                    .patch(update_inbox_record)
                    .delete(delete_inbox_record),
            )
            .route("/v1/inbox/{id}/defer", post(defer_inbox_record))
            .route("/v1/audio/speech", post(synthesize_speech))
            .route("/v1/audio/voices", get(list_voices))
            .route("/v1/ws", get(websocket))
            .layer(axum::middleware::from_fn_with_state(
                AuthState { token },
                authenticate,
            ))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
        let addr = listener.local_addr()?;
        let shutdown = tokio::signal::ctrl_c();

        let handle = tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    if let Ok(()) = shutdown.await {
                        tracing::info!("gateway shutting down");
                    }
                })
                .await;
            if let Err(e) = result {
                tracing::error!("gateway server error: {e}");
            }
        });

        Ok(Gateway {
            state,
            addr,
            handle,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn state(&self) -> &GatewayState {
        &self.state
    }

    /// Stop the server.
    pub async fn shutdown(self) {
        self.handle.abort();
        let _ = self;
    }
}

/// A handle that keeps the gateway's state reachable after it starts.
#[derive(Clone)]
pub struct GatewayHandle {
    pub state: GatewayState,
    pub addr: SocketAddr,
}

impl GatewayHandle {
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn ws_url(&self) -> String {
        format!("ws://{}/v1/ws", self.addr)
    }
}

/// Shared state for the auth middleware.
#[derive(Clone)]
struct AuthState {
    token: Option<String>,
}

/// Require the shared token when one is configured.
async fn authenticate(
    State(auth): State<AuthState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    // The page in the browser is open: it holds nothing secret, it is
    // only a drawing. Everything it draws still needs the token, and the
    // page asks for it. Its styles and scripts are part of the drawing.
    if request.method() == Method::GET {
        let path = request.uri().path();
        if path == "/" || path.starts_with("/assets/") {
            return next.run(request).await;
        }
    }

    let Some(expected) = &auth.token else {
        // No token configured: the gateway is unauthenticated, which is only
        // safe because it binds to localhost by default.
        return next.run(request).await;
    };

    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.strip_prefix("Bearer ").unwrap_or(v).to_string())
        .or_else(|| {
            headers
                .get("x-lia-token")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.to_string())
        })
        // A browser cannot put a header on a WebSocket, so the socket may
        // carry the token in its address. Nothing else may: the token
        // would end up in the addresses of the requests that are written
        // down.
        .or_else(|| socket_token(request.uri()));

    match provided {
        Some(token) if token == *expected => next.run(request).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse::new("invalid or missing token")),
        )
            .into_response(),
    }
}

/// The token a WebSocket carries in its address.
fn socket_token(uri: &Uri) -> Option<String> {
    if uri.path() != "/v1/ws" {
        return None;
    }
    let query = axum::extract::Query::<SocketToken>::try_from_uri(uri).ok()?;
    query.token.clone()
}

/// The token in the address of a WebSocket.
#[derive(Debug, Deserialize)]
struct SocketToken {
    #[serde(default)]
    token: Option<String>,
}

async fn health(State(state): State<GatewayState>) -> Json<HealthResponse> {
    let sessions = state.sessions.lock().await;
    Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        model: state.agent.provider().model().into(),
        provider: state.agent.provider().name().into(),
        sessions: sessions.len(),
        speech: state
            .voice
            .as_ref()
            .is_some_and(|voice| voice.can_synthesize()),
    })
}

/// The machine the gateway is running on, and the gateway itself.
///
/// The reading samples the CPU and the network for a moment, so it is taken
/// off the async runtime rather than holding up other requests.
async fn system_status(State(state): State<GatewayState>) -> Json<SystemStatus> {
    let system = tokio::task::spawn_blocking(crate::system::snapshot)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!("the system reading failed: {error}");
            crate::system::collect(std::time::Duration::ZERO)
        });
    let sessions = state.sessions.lock().await.len();
    let approval = match state.config.approval.policy {
        crate::tool::approval::ApprovalPolicy::Allow => "allow",
        crate::tool::approval::ApprovalPolicy::Deny => "deny",
        crate::tool::approval::ApprovalPolicy::Ask => "ask",
    };
    Json(SystemStatus {
        system,
        gateway: GatewayStatus {
            host: state.config.gateway.host.clone(),
            port: state.config.gateway.port,
            secure: state
                .config
                .gateway
                .token
                .as_deref()
                .map(|token| !token.trim().is_empty())
                .unwrap_or(false),
            approval: approval.into(),
            model: state.agent.provider().model().into(),
            provider: state.agent.provider().name().into(),
            sessions,
            tools: state.agent.tools().names().len(),
            skills: state.skills,
        },
    })
}

#[derive(Debug, Deserialize)]
struct InboxListQuery {
    kind: Option<InboxKind>,
    state: Option<InboxRecordState>,
    #[serde(default)]
    include_archived: bool,
}

#[derive(Debug, Deserialize)]
struct CaptureInboxRequest {
    text: String,
    #[serde(default = "default_inbox_kind")]
    kind: InboxKind,
    #[serde(default = "default_inbox_priority")]
    priority: InboxPriority,
    #[serde(default)]
    due_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Deserialize)]
struct UpdateInboxRequest {
    #[serde(default)]
    state: Option<InboxRecordState>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeferInboxRequest {
    until: chrono::DateTime<chrono::Utc>,
}

fn default_inbox_kind() -> InboxKind {
    InboxKind::Inbox
}

fn default_inbox_priority() -> InboxPriority {
    InboxPriority::Normal
}

fn inbox_error(error: InboxError) -> Response {
    let (status, message) = match error {
        InboxError::NotFound => (StatusCode::NOT_FOUND, "no such inbox record".to_string()),
        InboxError::Invalid(reason) => (StatusCode::BAD_REQUEST, reason.to_string()),
        other => (StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
    };
    (status, Json(ErrorResponse::new(message))).into_response()
}

fn local_inbox_owner() -> Owner {
    Owner::new(crate::inbox_identity::LOCAL_OWNER_ID)
        .expect("the built-in local inbox identity is valid")
}

async fn list_inbox(
    State(state): State<GatewayState>,
    Query(query): Query<InboxListQuery>,
) -> Json<Vec<InboxRecord>> {
    let filter = InboxFilter {
        kind: query.kind,
        state: query.state,
        include_archived: query.include_archived,
    };
    let inbox = state.inbox.lock().await;
    Json(inbox.list(&local_inbox_owner(), &filter))
}

async fn capture_inbox(
    State(state): State<GatewayState>,
    Json(request): Json<CaptureInboxRequest>,
) -> Response {
    let new = NewRecord {
        kind: request.kind,
        text: request.text,
        priority: request.priority,
        due_at: request.due_at,
        snoozed_until: None,
        source: Default::default(),
        provenance: Provenance::User,
    };
    let mut inbox = state.inbox.lock().await;
    match inbox.create(&local_inbox_owner(), new) {
        Ok(record) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(error) => inbox_error(error),
    }
}

async fn get_inbox_record(
    State(state): State<GatewayState>,
    Path(id): Path<uuid::Uuid>,
) -> Response {
    let inbox = state.inbox.lock().await;
    match inbox.get(&local_inbox_owner(), id) {
        Ok(record) => Json(record).into_response(),
        Err(error) => inbox_error(error),
    }
}

async fn update_inbox_record(
    State(state): State<GatewayState>,
    Path(id): Path<uuid::Uuid>,
    Json(request): Json<UpdateInboxRequest>,
) -> Response {
    let mut inbox = state.inbox.lock().await;
    match inbox.update(
        &local_inbox_owner(),
        id,
        InboxUpdate {
            state: request.state,
            text: request.text,
            ..InboxUpdate::default()
        },
    ) {
        Ok(record) => Json(record).into_response(),
        Err(error) => inbox_error(error),
    }
}

async fn defer_inbox_record(
    State(state): State<GatewayState>,
    Path(id): Path<uuid::Uuid>,
    Json(request): Json<DeferInboxRequest>,
) -> Response {
    let mut inbox = state.inbox.lock().await;
    match inbox.update(
        &local_inbox_owner(),
        id,
        InboxUpdate {
            snoozed_until: Some(Some(request.until)),
            ..InboxUpdate::default()
        },
    ) {
        Ok(record) => Json(record).into_response(),
        Err(error) => inbox_error(error),
    }
}

async fn delete_inbox_record(
    State(state): State<GatewayState>,
    Path(id): Path<uuid::Uuid>,
) -> Response {
    let mut inbox = state.inbox.lock().await;
    match inbox.delete(&local_inbox_owner(), id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => inbox_error(error),
    }
}

async fn list_models(State(state): State<GatewayState>) -> Response {
    match state.agent.provider().list_models().await {
        Ok(models) => Json(models).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(ErrorResponse::new(e.to_string())),
        )
            .into_response(),
    }
}

/// The tools this gateway's assistant can call.
async fn list_tools(State(state): State<GatewayState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "tools": state.agent.tools().names() }))
}

/// The skills that were found for this gateway's configuration.
async fn list_skills(State(state): State<GatewayState>) -> Json<serde_json::Value> {
    let skills = state.config.discover_skills();
    let list: Vec<serde_json::Value> = skills
        .iter()
        .map(|skill| {
            serde_json::json!({
                "name": skill.name,
                "description": skill.description,
                "source": skill.source.as_str(),
            })
        })
        .collect();
    Json(serde_json::json!({ "skills": list }))
}

/// The editable settings, as a JSON object.
fn config_json(config: &Config, path: &std::path::Path) -> serde_json::Value {
    let settings: serde_json::Map<String, serde_json::Value> = config
        .settings()
        .into_iter()
        .map(|(key, value)| (key.to_string(), serde_json::Value::String(value)))
        .collect();
    serde_json::json!({
        "config": settings,
        "path": path.display().to_string(),
        "restart_required": true,
    })
}

/// Read the settings that the interface is allowed to show.
async fn get_config(State(state): State<GatewayState>) -> Response {
    let path = state.config.configured_path();
    match Config::load_file(&path) {
        Ok(config) => Json(config_json(&config, &path)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new(format!(
                "the configuration could not be read: {e}"
            ))),
        )
            .into_response(),
    }
}

/// Change settings in the configuration file.
async fn update_config(
    State(state): State<GatewayState>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(changes) = body.as_object() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new("the configuration must be an object")),
        )
            .into_response();
    };
    let path = state.config.configured_path();
    let mut config = match Config::load_file(&path) {
        Ok(config) => config,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(format!(
                    "the configuration could not be read: {e}"
                ))),
            )
                .into_response();
        }
    };

    for (key, value) in changes {
        let text = match value {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Bool(flag) => flag.to_string(),
            serde_json::Value::Number(number) => number.to_string(),
            serde_json::Value::Null => String::new(),
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse::new(format!(
                        "the value of '{key}' must be text, a number or a switch"
                    ))),
                )
                    .into_response();
            }
        };
        if let Err(e) = config.set_setting(key, &text) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse::new(e.to_string())),
            )
                .into_response();
        }
    }

    if let Err(e) = config.save(&path) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new(format!(
                "the configuration could not be saved: {e}"
            ))),
        )
            .into_response();
    }
    Json(config_json(&config, &path)).into_response()
}

fn summarize(session: &Session) -> SessionSummary {
    SessionSummary {
        id: session.id.clone(),
        title: session.title.clone(),
        source: session.source.clone(),
        created_at: session.created_at.to_rfc3339(),
        updated_at: session.updated_at.to_rfc3339(),
        message_count: session.messages.len(),
    }
}

async fn list_sessions(State(state): State<GatewayState>) -> Json<Vec<SessionSummary>> {
    let sessions = state.sessions.lock().await;
    Json(sessions.list().into_iter().map(summarize).collect())
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

async fn create_session(
    State(state): State<GatewayState>,
    body: Option<Json<CreateSessionRequest>>,
) -> Response {
    let request = body.map(|Json(b)| b).unwrap_or(CreateSessionRequest {
        id: None,
        source: None,
    });

    let id = request
        .id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let source = request.source.unwrap_or_else(|| "api".to_string());

    let mut sessions = state.sessions.lock().await;
    let session = Session::new(&id, &source);
    sessions.insert(session.clone());
    if let Err(e) = sessions.persist(&session) {
        tracing::warn!("could not persist session {id}: {e}");
    }
    (StatusCode::CREATED, Json(summarize(&session))).into_response()
}

async fn get_session(State(state): State<GatewayState>, Path(id): Path<String>) -> Response {
    let sessions = state.sessions.lock().await;
    match sessions.get(&id) {
        Some(session) => Json(session).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new("no such session")),
        )
            .into_response(),
    }
}

async fn delete_session(State(state): State<GatewayState>, Path(id): Path<String>) -> Response {
    let mut sessions = state.sessions.lock().await;
    match sessions.remove(&id) {
        Some(_) => StatusCode::NO_CONTENT.into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new("no such session")),
        )
            .into_response(),
    }
}

async fn send_message(
    State(state): State<GatewayState>,
    Path(id): Path<String>,
    Json(request): Json<SendMessageRequest>,
) -> Response {
    // Collect every event the turn produces so the HTTP caller gets the whole
    // trace, not just the final text.
    let (tx, mut rx) = mpsc::channel(256);
    let runner = state.runner.clone();
    let session_id = id.clone();
    let content = request.content.clone();

    // The message may carry pictures as well as words.
    let message = if request.images.is_empty() {
        crate::provider::types::Message::user(content)
    } else {
        let blocks: Vec<crate::provider::types::ContentBlock> = request
            .images
            .iter()
            .map(|image| image.to_block())
            .collect();
        crate::provider::types::Message::user_with_images(content, blocks)
    };

    let handle = tokio::spawn(async move {
        runner.run_message(&session_id, Some(message), tx).await;
    });

    let mut events = Vec::new();
    let mut reply = String::new();
    let mut usage = None;
    let mut iterations = 0;

    while let Some(event) = rx.recv().await {
        match &event {
            AgentEvent::TextDelta(text) => reply.push_str(text),
            AgentEvent::TurnComplete { text, .. } if !text.is_empty() => {
                if reply.is_empty() {
                    reply = text.clone();
                }
            }
            AgentEvent::Finished {
                usage: u,
                iterations: i,
                ..
            } => {
                usage = *u;
                iterations = *i;
            }
            _ => {}
        }
        events.push(WireEvent::from(event));
    }

    let _ = handle.await;

    // Persist the updated session.
    {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(&id) {
            session.derive_title();
            session.touch();
            let snapshot = session.clone();
            if let Err(e) = sessions.persist(&snapshot) {
                tracing::warn!("could not persist session {id}: {e}");
            }
        }
    }

    Json(SendMessageResponse {
        session_id: id,
        reply,
        events,
        usage,
        iterations,
    })
    .into_response()
}

/// Request body for the speech synthesis endpoint.
#[derive(Deserialize)]
struct SpeechRequest {
    /// The text to synthesize.
    input: String,
    /// The voice to speak in. The configured voice is used when this is not
    /// set, and a name from the Phonon voice catalog can be asked for here.
    #[serde(default)]
    voice: Option<String>,
    /// `wav` (the default) or `pcm`. The rest of the OpenAI shape is
    /// accepted and ignored, so an existing client needs no changes.
    #[serde(default)]
    response_format: Option<String>,
}

/// List the voices available on the in-process Phonon model.
///
/// Returns a JSON object with a `voices` array, each entry carrying the
/// voice `name`. The checkpoint's own voices come first, then those of the
/// voice catalog. Returns an empty list when Phonon is not active.
async fn list_voices(State(state): State<GatewayState>) -> Json<serde_json::Value> {
    let voices: Vec<serde_json::Value> = match &state.voice {
        Some(voice) => {
            let voice = voice.clone();
            tokio::task::spawn_blocking(move || {
                voice
                    .phonon_voices()
                    .into_iter()
                    .map(|name| serde_json::json!({ "name": name }))
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default()
        }
        None => Vec::new(),
    };
    Json(serde_json::json!({ "voices": voices }))
}

/// Synthesize speech in the configured voice.
///
/// The shape is the OpenAI `/v1/audio/speech` endpoint's: `input` is the
/// text, `voice` selects a voice, and `response_format` is `wav` or `pcm`.
/// The configured voice is used when the request does not name one. Both the
/// built-in Phonon model and a program that writes a wav, such as piper, can
/// be served, so the browser hears the same voice as the terminal; `pcm` is
/// only available from Phonon. Returns a 501 when no synthesizer is
/// available.
async fn synthesize_speech(
    State(state): State<GatewayState>,
    Json(body): Json<SpeechRequest>,
) -> Response {
    let speaker = match &state.voice {
        Some(voice) => voice.clone(),
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(serde_json::json!({
                    "error": "no speech synthesizer is available; enable the voice and install a Phonon model or a program that writes a wav"
                })),
            )
                .into_response();
        }
    };

    let text = body.input.trim().to_string();
    if text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "the input is empty" })),
        )
            .into_response();
    }

    let format = body
        .response_format
        .as_deref()
        .unwrap_or("wav")
        .trim()
        .to_ascii_lowercase();
    if format != "wav" && format != "pcm" {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("the response format '{format}' is not supported; use wav or pcm")
            })),
        )
            .into_response();
    }
    if format == "pcm" && !speaker.can_synthesize_pcm() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "pcm is only available from the built-in Phonon voice; use wav"
            })),
        )
            .into_response();
    }

    let voice = body.voice.clone();
    if format == "pcm" {
        let result = tokio::task::spawn_blocking(move || {
            let (pcm, rate) = speaker.synthesize_with(&text, voice.as_deref())?;
            Ok::<_, anyhow::Error>((pcm16_bytes(&pcm), rate))
        })
        .await;
        return match result {
            Ok(Ok((audio, rate))) => (
                StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, "audio/pcm".to_string()),
                    (
                        axum::http::HeaderName::from_static("x-sample-rate"),
                        rate.to_string(),
                    ),
                ],
                audio,
            )
                .into_response(),
            Ok(Err(e)) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response(),
        };
    }

    match speaker.synthesize_wav_async(&text, voice.as_deref()).await {
        Ok(audio) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "audio/wav".to_string())],
            audio,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// Mono 16-bit little-endian samples, which is what `pcm` means here.
fn pcm16_bytes(pcm: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pcm.len() * 2);
    for sample in pcm {
        let sample = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

/// Upgrade a WebSocket connection and run the streaming protocol.
async fn websocket(State(state): State<GatewayState>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Where a handler sends its output.
///
/// The original protocol sends its messages as they are. The envelope turns
/// them into the answer to a request and the events that came before it.
#[derive(Clone)]
enum Sink {
    /// The original protocol.
    Plain(mpsc::Sender<OutFrame>),
    /// The request/response envelope.
    Envelope {
        /// The id of the request being answered.
        id: String,
        tx: mpsc::Sender<OutFrame>,
    },
}

impl Sink {
    /// Send a message to the client.
    async fn send(&self, message: ServerMessage) {
        let frame = match self {
            Sink::Plain(_) => OutFrame::Message(message),
            Sink::Envelope { id, .. } => {
                let id = id.clone();
                match message {
                    ServerMessage::Event { event } => {
                        let (name, data) = split_event(event);
                        OutFrame::Event(EventFrame::new(name, data))
                    }
                    ServerMessage::Done {
                        session_id,
                        reply,
                        usage,
                        iterations,
                    } => OutFrame::Response(ResponseFrame::ok(
                        id,
                        serde_json::json!({
                            "sessionId": session_id,
                            "reply": reply,
                            "usage": usage,
                            "iterations": iterations,
                        }),
                    )),
                    ServerMessage::Error { message } => {
                        OutFrame::Response(ResponseFrame::failed(id, message))
                    }
                    ServerMessage::Sessions { sessions } => OutFrame::Response(ResponseFrame::ok(
                        id,
                        serde_json::json!({ "sessions": sessions }),
                    )),
                    ServerMessage::SessionCreated { session } => OutFrame::Response(
                        ResponseFrame::ok(id, serde_json::json!({ "session": session })),
                    ),
                    ServerMessage::Health { health } => OutFrame::Response(ResponseFrame::ok(
                        id,
                        serde_json::to_value(health).unwrap_or(serde_json::Value::Null),
                    )),
                    other => OutFrame::Message(other),
                }
            }
        };
        let _ = self.tx().send(frame).await;
    }

    fn tx(&self) -> &mpsc::Sender<OutFrame> {
        match self {
            Sink::Plain(tx) => tx,
            Sink::Envelope { tx, .. } => tx,
        }
    }
}

/// Handle one WebSocket connection.
async fn handle_socket(socket: WebSocket, state: GatewayState) {
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::channel::<OutFrame>(256);

    // Whether this client speaks the request/response envelope. It is set
    // the first time a request arrives.
    let envelope = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // This connection's permission requests. Only the client that started
    // the turn can answer them.
    let approver = GatewayApprover::new(out_tx.clone(), envelope.clone());

    // Greet the client.
    let _ = out_tx
        .send(OutFrame::Message(ServerMessage::Hello {
            version: env!("CARGO_PKG_VERSION").into(),
            model: state.agent.provider().model().into(),
        }))
        .await;

    loop {
        tokio::select! {
            // Outbound: events from the broadcaster and turn results.
            Some(frame) = out_rx.recv() => {
                let text = match serde_json::to_string(&frame) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!("could not serialize gateway message: {e}");
                        continue;
                    }
                };
                if sink.send(WsMessage::Text(text.into())).await.is_err() {
                    break;
                }
            }
            // Inbound: a frame from the client.
            Some(Ok(frame)) = stream.next() => {
                match frame {
                    WsMessage::Text(text) => {
                        let parsed: ClientMessage = match serde_json::from_str(&text) {
                            Ok(m) => m,
                            Err(e) => {
                                let _ = out_tx.send(OutFrame::Message(ServerMessage::Error {
                                    message: format!("malformed message: {e}"),
                                })).await;
                                continue;
                            }
                        };
                        match parsed {
                            ClientMessage::Request { id, method, params } => {
                                envelope.store(true, std::sync::atomic::Ordering::Relaxed);
                                handle_request(id, method, params, &state, &out_tx, &approver).await;
                            }
                            other => {
                                let sink = Sink::Plain(out_tx.clone());
                                if handle_client_message(other, &state, &sink, &approver).await {
                                    break;
                                }
                            }
                        }
                    }
                    WsMessage::Close(_) => break,
                    WsMessage::Ping(payload) => {
                        let _ = sink.send(WsMessage::Pong(payload)).await;
                    }
                    _ => {}
                }
            }
            else => break,
        }
    }
}

/// Answer a request in the request/response envelope.
///
/// The request is turned into the frame the handlers already speak, so both
/// protocols run the same code.
async fn handle_request(
    id: String,
    method: String,
    params: serde_json::Value,
    state: &GatewayState,
    out_tx: &mpsc::Sender<OutFrame>,
    approver: &GatewayApprover,
) {
    // A request may leave out its parameters altogether.
    let params = if params.is_null() {
        serde_json::json!({})
    } else {
        params
    };

    let params: RequestParams = match serde_json::from_value(params) {
        Ok(params) => params,
        Err(e) => {
            let _ = out_tx
                .send(OutFrame::Response(ResponseFrame::failed(
                    id,
                    format!("the parameters are not right: {e}"),
                )))
                .await;
            return;
        }
    };

    let client = match method.as_str() {
        "message.send" => match params.content {
            Some(content) => ClientMessage::Send {
                content,
                session_id: params.session_id,
                images: params.images.unwrap_or_default(),
            },
            None => {
                let _ = out_tx
                    .send(OutFrame::Response(ResponseFrame::failed(
                        id,
                        "message.send needs a 'content'",
                    )))
                    .await;
                return;
            }
        },
        "message.approve" => match params.request_id {
            Some(request_id) => ClientMessage::Approve {
                request_id,
                always: params.always.unwrap_or(false),
            },
            None => {
                let _ = out_tx
                    .send(OutFrame::Response(ResponseFrame::failed(
                        id,
                        "message.approve needs a 'requestId'",
                    )))
                    .await;
                return;
            }
        },
        "message.deny" => match params.request_id {
            Some(request_id) => ClientMessage::Deny { request_id },
            None => {
                let _ = out_tx
                    .send(OutFrame::Response(ResponseFrame::failed(
                        id,
                        "message.deny needs a 'requestId'",
                    )))
                    .await;
                return;
            }
        },
        "session.list" => ClientMessage::ListSessions,
        "session.get" => match params.session_id {
            Some(session_id) => ClientMessage::Subscribe { session_id },
            None => {
                let _ = out_tx
                    .send(OutFrame::Response(ResponseFrame::failed(
                        id,
                        "session.get needs a 'sessionId'",
                    )))
                    .await;
                return;
            }
        },
        "session.create" => ClientMessage::CreateSession,
        "health" => ClientMessage::Health,
        other => {
            let _ = out_tx
                .send(OutFrame::Response(ResponseFrame::failed(
                    id,
                    format!("there is no method called '{other}'"),
                )))
                .await;
            return;
        }
    };

    let sink = Sink::Envelope {
        id,
        tx: out_tx.clone(),
    };
    handle_client_message(client, state, &sink, approver).await;
}

/// Process one client frame. Returns `true` when the socket should close.
async fn handle_client_message(
    message: ClientMessage,
    state: &GatewayState,
    sink: &Sink,
    approver: &GatewayApprover,
) -> bool {
    match message {
        ClientMessage::Send {
            content,
            session_id,
            images,
        } => {
            let session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

            // This connection is the one that can grant permission for the
            // tools this turn uses.
            state
                .runner
                .set_approver(&session_id, Arc::new(approver.clone()))
                .await;

            // The turn runs in the background. If it were awaited here, the
            // socket could not read the client's answer to a permission
            // request, and a tool waiting for permission would deadlock.
            let runner = state.runner.clone();
            let sessions = state.sessions.clone();
            let sink = sink.clone();
            let id = session_id.clone();

            tokio::spawn(async move {
                // Give this turn its own event channel and forward it to the
                // socket.
                let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
                let forward_sink = sink.clone();
                let forwarder = tokio::spawn(async move {
                    let mut usage = None;
                    let mut iterations = 0;
                    while let Some(event) = rx.recv().await {
                        if let AgentEvent::Finished {
                            usage: used,
                            iterations: taken,
                            ..
                        } = &event
                        {
                            usage = *used;
                            iterations = *taken;
                        }
                        let wire = ServerMessage::Event {
                            event: WireEvent::from(event),
                        };
                        forward_sink.send(wire).await;
                    }
                    (usage, iterations)
                });

                let message = if images.is_empty() {
                    crate::provider::types::Message::user(content)
                } else {
                    let blocks: Vec<crate::provider::types::ContentBlock> =
                        images.iter().map(|image| image.to_block()).collect();
                    crate::provider::types::Message::user_with_images(content, blocks)
                };
                runner.run_message(&id, Some(message), tx).await;

                // Every event has been forwarded before the turn is reported
                // as done.
                let (usage, iterations) = forwarder.await.unwrap_or((None, 0));

                let sessions = sessions.lock().await;
                let summary = sessions
                    .get(&session_id)
                    .map(|s| {
                        let last_text = s
                            .messages
                            .iter()
                            .rev()
                            .find(|m| {
                                m.role == crate::provider::types::Role::Assistant
                                    && m.content.iter().any(|b| b.is_text())
                            })
                            .map(|m| m.text())
                            .unwrap_or_default();
                        (s.messages.len(), last_text)
                    })
                    .unwrap_or((0, String::new()));

                let _ = sink
                    .send(ServerMessage::Done {
                        session_id,
                        reply: summary.1,
                        usage,
                        iterations,
                    })
                    .await;
            });

            false
        }
        ClientMessage::Approve { request_id, always } => {
            let decision = if always {
                ApprovalDecision::ApproveAlways
            } else {
                ApprovalDecision::Approve
            };
            if !approver.resolve(&request_id, decision).await {
                let _ = sink
                    .send(ServerMessage::Error {
                        message: format!(
                            "no tool call is waiting for approval under '{request_id}'"
                        ),
                    })
                    .await;
            }
            false
        }
        ClientMessage::Deny { request_id } => {
            if !approver.resolve(&request_id, ApprovalDecision::Deny).await {
                let _ = sink
                    .send(ServerMessage::Error {
                        message: format!(
                            "no tool call is waiting for approval under '{request_id}'"
                        ),
                    })
                    .await;
            }
            false
        }
        ClientMessage::Subscribe { session_id } => {
            let sessions = state.sessions.lock().await;
            let list: Vec<SessionSummary> = sessions
                .get(&session_id)
                .map(|s| vec![summarize(s)])
                .unwrap_or_default();
            let _ = sink.send(ServerMessage::Sessions { sessions: list }).await;
            false
        }
        ClientMessage::ListSessions => {
            let sessions = state.sessions.lock().await;
            let list: Vec<SessionSummary> = sessions.list().into_iter().map(summarize).collect();
            let _ = sink.send(ServerMessage::Sessions { sessions: list }).await;
            false
        }
        ClientMessage::CreateSession => {
            let mut sessions = state.sessions.lock().await;
            let id = uuid::Uuid::new_v4().to_string();
            let summary = sessions.get_or_create(&id).map(|s| summarize(s));
            if let Some(summary) = summary {
                let _ = sink
                    .send(ServerMessage::SessionCreated { session: summary })
                    .await;
            }
            false
        }
        ClientMessage::Health => {
            let sessions = state.sessions.lock().await;
            let health = HealthResponse {
                status: "ok".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                model: state.agent.provider().model().into(),
                provider: state.agent.provider().name().into(),
                sessions: sessions.len(),
                speech: state
                    .voice
                    .as_ref()
                    .is_some_and(|voice| voice.can_synthesize()),
            };
            let _ = sink.send(ServerMessage::Health { health }).await;
            false
        }
        ClientMessage::Shutdown => true,
        ClientMessage::Request { .. } => false,
    }
}

/// An approver that waits for the user to answer over the gateway.
///
/// Each connection has its own approver, so a permission request goes to the
/// client that asked for it and only that client can answer it.
#[derive(Clone)]
pub struct GatewayApprover {
    pending: Arc<
        Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<ApprovalDecision>>>,
    >,
    next_id: Arc<std::sync::atomic::AtomicU64>,
    out_tx: mpsc::Sender<OutFrame>,
    /// Whether the client speaks the request/response envelope.
    envelope: Arc<std::sync::atomic::AtomicBool>,
}

impl GatewayApprover {
    pub fn new(
        out_tx: mpsc::Sender<OutFrame>,
        envelope: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        GatewayApprover {
            pending: Arc::new(Mutex::new(std::collections::HashMap::new())),
            next_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            out_tx,
            envelope,
        }
    }

    /// Answer a pending request.
    pub async fn resolve(&self, request_id: &str, decision: ApprovalDecision) -> bool {
        let mut pending = self.pending.lock().await;
        if let Some(sender) = pending.remove(request_id) {
            let _ = sender.send(decision);
            true
        } else {
            false
        }
    }
}

#[async_trait::async_trait]
impl crate::agent::ApprovalHandler for GatewayApprover {
    async fn request(&self, request: ApprovalRequest) -> Option<ApprovalDecision> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.pending.lock().await.insert(id.clone(), tx);

        // The request goes out in the shape the client understands: a
        // message of its own, or an event in the envelope.
        let frame = if self.envelope.load(std::sync::atomic::Ordering::Relaxed) {
            OutFrame::Event(EventFrame::new(
                "approval_request",
                serde_json::json!({
                    "requestId": id,
                    "tool": request.tool,
                    "summary": request.summary,
                    "arguments": request.arguments,
                }),
            ))
        } else {
            OutFrame::Message(ServerMessage::ApprovalRequest {
                request_id: id,
                tool: request.tool,
                summary: request.summary,
                arguments: request.arguments,
            })
        };

        let _ = self.out_tx.send(frame).await;

        // If the client goes away, the sender is dropped and this returns
        // `None`, which the agent treats as a refusal.
        rx.await.ok()
    }
}

/// Query parameters accepted by list endpoints.
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub source: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_is_written_as_little_endian_sixteen_bit() {
        let bytes = pcm16_bytes(&[0.0, 1.0, -1.0]);
        assert_eq!(bytes.len(), 6);
        assert_eq!(&bytes[0..2], &0i16.to_le_bytes());
        assert_eq!(&bytes[2..4], &32767i16.to_le_bytes());
        assert_eq!(&bytes[4..6], &(-32767i16).to_le_bytes());
    }

    #[test]
    fn a_speech_request_accepts_the_rest_of_the_openai_shape() {
        let request: SpeechRequest = serde_json::from_value(serde_json::json!({
            "model": "tts-1",
            "input": "hello",
            "voice": "Juniper",
            "response_format": "pcm",
            "speed": 1.0,
        }))
        .expect("the request is read");
        assert_eq!(request.input, "hello");
        assert_eq!(request.voice.as_deref(), Some("Juniper"));
        assert_eq!(request.response_format.as_deref(), Some("pcm"));
    }
}
