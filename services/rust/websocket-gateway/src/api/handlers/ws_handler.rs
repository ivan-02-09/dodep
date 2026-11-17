use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::auth::{token_from_handshake, TokenError, TokenVerifier};
use crate::domain::channel::{parse_topic, ClientMessage, ServerMessage, Topic, TopicResponse};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct WsConnectParams {
    /// Optional so that a missing or empty token is answered with 401
    /// UNAUTHORIZED by this handler rather than a 400 from the query extractor.
    token: Option<String>,
}

/// WebSocket upgrade handler
///
/// Both extractors are optional on purpose: `WebSocketUpgrade` rejects with
/// 400 when the `Connection: upgrade` header is absent, and `Query` rejects
/// when the query string cannot be deserialized. Making them optional lets the
/// handler run its own checks and answer 401 for a missing/empty token,
/// independent of whether the client actually attempted an upgrade.
pub async fn ws_handler(
    ws: Option<WebSocketUpgrade>,
    State(state): State<AppState>,
    headers: HeaderMap,
    params: Option<Query<WsConnectParams>>,
) -> Response {
    // Authenticate before anything else: the user id decides which private
    // topics (`user_bets:<id>`, `user_balance:<id>`) the socket may see.
    let query_token = params.and_then(|Query(p)| p.token);
    let token = match token_from_handshake(&headers, query_token.as_deref()) {
        Some(token) => token,
        None => return (StatusCode::UNAUTHORIZED, "Missing access token").into_response(),
    };

    let verifier = match TokenVerifier::from_base64(&state.config().auth_ed25519_public_key) {
        Ok(verifier) => verifier,
        Err(err) => {
            // Fail closed: an unconfigured key must not mean "trust everyone".
            tracing::error!(
                error = %err,
                "websocket handshake rejected: token verification unavailable"
            );
            return (StatusCode::SERVICE_UNAVAILABLE, "Authentication unavailable").into_response();
        }
    };

    let user_id = match verifier.verify(&token) {
        Ok(user) => user.user_id,
        Err(TokenError::Missing) => {
            return (StatusCode::UNAUTHORIZED, "Missing access token").into_response()
        }
        Err(err) => {
            tracing::debug!(error = %err, "websocket handshake rejected: invalid token");
            return (StatusCode::UNAUTHORIZED, "Invalid access token").into_response();
        }
    };

    let Some(ws) = ws else {
        return (
            StatusCode::BAD_REQUEST,
            "Not a WebSocket upgrade request",
        )
            .into_response();
    };

    // Enforce the global connection cap before completing the handshake, so an
    // over-capacity client sees 503 rather than a socket closed right after 101.
    let sub_mgr = state.subscription_manager();
    let current = sub_mgr.connection_count();
    let max_conns = state.config().max_connections;
    if current >= max_conns {
        tracing::warn!(current, max_conns, "Connection limit reached");
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "Server at capacity",
        )
            .into_response();
    }

    ws.on_upgrade(move |socket| handle_connection(socket, state, user_id))
}

async fn handle_connection(socket: WebSocket, state: AppState, user_id: i64) {
    let config = state.config();

    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Message>(config.channel_buffer_size);

    let sub_mgr = state.subscription_manager();
    let max_subs = config.max_subscriptions_per_connection;

    // Track connection
    sub_mgr.increment_connections();
    tracing::info!(user_id, "WebSocket connected");

    // Auto-subscribe to user-specific topics
    if user_id > 0 {
        sub_mgr.subscribe(Topic::UserBets(user_id), tx.clone());
        sub_mgr.subscribe(Topic::UserBalance(user_id), tx.clone());
    }

    // Task 1: Forward messages from channel to WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Task 2: Read client messages (subscriptions, pings)
    let tx_clone = tx.clone();
    let sub_mgr_clone = sub_mgr.clone();
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(text) => {
                    handle_client_message(&text, &sub_mgr_clone, &tx_clone, user_id, max_subs)
                        .await;
                }
                Message::Ping(data) => {
                    let _ = tx_clone.send(Message::Pong(data)).await;
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Task 3: Listen for Kafka broadcasts and forward to this client
    let kafka_task = {
        let mut kafka_rx = state.kafka_receiver();
        let tx_clone = tx.clone();
        tokio::spawn(async move {
            while let Ok(msg) = kafka_rx.recv().await {
                let _ = tx_clone.send(Message::Binary(msg.payload)).await;
            }
        })
    };

    // Wait for any task to finish (connection close)
    tokio::select! {
        _ = send_task => {}
        _ = recv_task => {}
        _ = kafka_task => {}
    }

    // Cleanup
    if user_id > 0 {
        sub_mgr.unsubscribe(&Topic::UserBets(user_id), &tx);
        sub_mgr.unsubscribe(&Topic::UserBalance(user_id), &tx);
    }
    sub_mgr.decrement_connections();

    tracing::info!(user_id, "WebSocket disconnected");
}

/// Private (`user_*`) topics are only visible to their owner; market data
/// topics are public.
fn is_topic_visible_to(topic: &Topic, user_id: i64) -> bool {
    match topic {
        Topic::UserBets(id) | Topic::UserBalance(id) => *id == user_id,
        Topic::EventOdds(_) | Topic::EventStats(_) | Topic::SportScores(_) => true,
    }
}

fn send_error(sender: &mpsc::Sender<Message>, code: &str, message: &str) {
    let _ = sender.try_send(Message::Text(
        serde_json::to_string(&ServerMessage::Error {
            code: code.into(),
            message: message.into(),
        })
        .unwrap_or_default(),
    ));
}

async fn handle_client_message(
    text: &str,
    sub_mgr: &crate::infrastructure::connection_manager::SubscriptionManager,
    sender: &mpsc::Sender<Message>,
    user_id: i64,
    max_subs: usize,
) {
    let msg: ClientMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(_) => {
            let _ = sender.try_send(Message::Text(
                serde_json::to_string(&ServerMessage::Error {
                    code: "WS_INVALID_MESSAGE".into(),
                    message: "Invalid JSON".into(),
                })
                .unwrap_or_default(),
            ));
            return;
        }
    };

    match msg {
        ClientMessage::Subscribe { topics } => {
            let mut subscribed = Vec::new();
            let current = sub_mgr.total_subscriptions();
            let allowed = max_subs.saturating_sub(current).min(topics.len());

            for topic_req in topics.into_iter().take(allowed) {
                if let Some(topic) = parse_topic(&topic_req) {
                    // Never let a client subscribe to another user's private
                    // streams, whatever the token claims.
                    if !is_topic_visible_to(&topic, user_id) {
                        tracing::warn!(
                            user_id,
                            topic = %topic,
                            "rejected subscription to foreign topic"
                        );
                        send_error(sender, "WS_FORBIDDEN_TOPIC", "Not your topic");
                        continue;
                    }
                    sub_mgr.subscribe(topic, sender.clone());
                    subscribed.push(TopicResponse {
                        topic_type: topic_req.topic_type,
                        id: topic_req.id,
                    });
                }
            }

            let _ = sender.try_send(Message::Text(
                serde_json::to_string(&ServerMessage::Subscribed { topics: subscribed })
                    .unwrap_or_default(),
            ));
        }
        ClientMessage::Unsubscribe { topics } => {
            let mut unsubscribed = Vec::new();
            for topic_req in topics {
                if let Some(topic) = parse_topic(&topic_req) {
                    if !is_topic_visible_to(&topic, user_id) {
                        send_error(sender, "WS_FORBIDDEN_TOPIC", "Not your topic");
                        continue;
                    }
                    sub_mgr.unsubscribe(&topic, sender);
                    unsubscribed.push(TopicResponse {
                        topic_type: topic_req.topic_type,
                        id: topic_req.id,
                    });
                }
            }

            let _ = sender.try_send(Message::Text(
                serde_json::to_string(&ServerMessage::Unsubscribed {
                    topics: unsubscribed,
                })
                .unwrap_or_default(),
            ));
        }
        ClientMessage::Ping => {
            let _ = sender.try_send(Message::Text(
                serde_json::to_string(&ServerMessage::Pong).unwrap_or_default(),
            ));
        }
    }
}
