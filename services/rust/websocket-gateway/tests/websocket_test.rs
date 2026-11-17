//! Integration tests for WebSocket Gateway.
//!
//! The HTTP surface (liveness/readiness and the pre-upgrade auth gate) is
//! exercised in-process against the real router. Everything that would need a
//! live Redpanda broker or a 100k-connection load generator is covered at the
//! unit level instead (topic parsing/routing, subscription accounting and
//! broadcast fan-out), so these tests stay hermetic.

use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine as _;
use jwt_simple::algorithms::{Ed25519KeyPair, EdDSAKeyPairLike};
use jwt_simple::prelude::Duration as JwtDuration;
use tokio_tungstenite::tungstenite;
use futures::{SinkExt, StreamExt};
use tower::ServiceExt;
use websocket_gateway::api;
use websocket_gateway::config::AppConfig;
use websocket_gateway::domain::channel::{
    kafka_to_topic, parse_topic, ClientMessage, Topic, TopicRequest, KAFKA_TOPICS,
};
use websocket_gateway::infrastructure::connection_manager::SubscriptionManager;
use websocket_gateway::infrastructure::kafka_consumer::KafkaBroadcaster;
use websocket_gateway::state::AppState;

/// Unused-broker config: `ClientConfig::create` and `subscribe` are lazy, so no
/// connection is attempted while these tests run.
const TEST_BROKERS: &str = "127.0.0.1:1";

/// Test key the gateway verifies handshake tokens against.
fn test_key_pair() -> Ed25519KeyPair {
    Ed25519KeyPair::generate()
}

fn public_key_b64(pair: &Ed25519KeyPair) -> String {
    base64::engine::general_purpose::STANDARD.encode(pair.public_key().to_bytes())
}

/// Sign an access token the way the Go auth service does: `iss`, `sub` and a
/// `token_type` custom claim.
fn access_token(pair: &Ed25519KeyPair, user_id: &str) -> String {
    let claims = jwt_simple::claims::Claims::with_custom_claims(
        serde_json::json!({ "token_type": "access" }),
        JwtDuration::from_secs(900),
    )
    .with_issuer("opus-casino-auth")
    .with_subject(user_id);
    pair.sign(claims).expect("test token signs")
}

fn test_config(public_key: &str) -> AppConfig {
    AppConfig {
        service_name: "websocket-gateway-test".into(),
        http_port: 0,
        redpanda_brokers: TEST_BROKERS.into(),
        cache_url: "redis://127.0.0.1:1".into(),
        auth_ed25519_public_key: public_key.to_string(),
        max_connections: 2,
        channel_buffer_size: 8,
        max_subscriptions_per_connection: 3,
    }
}

fn test_app_with(public_key: &str) -> axum::Router {
    let config = test_config(public_key);
    let broadcaster = KafkaBroadcaster::new(&config.redpanda_brokers)
        .expect("KafkaBroadcaster::new must not require a live broker");
    api::build(AppState::new(config, broadcaster))
}

/// Router configured with a freshly generated verification key.
fn test_app() -> (axum::Router, Ed25519KeyPair) {
    let pair = test_key_pair();
    (test_app_with(&public_key_b64(&pair)), pair)
}

#[tokio::test]
async fn health_endpoints_return_ok() {
    let (app, _key) = test_app();

    for path in ["/healthz", "/readyz"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{path} must report healthy"
        );
    }
}

#[tokio::test]
async fn ws_upgrade_requires_token() {
    let (app, _key) = test_app();

    // Missing and empty tokens are both refused with 401 by the auth gate,
    // before any upgrade happens.
    for uri in ["/ws", "/ws?token=", "/ws?token=&other=1"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body reads");
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{uri} must be rejected before upgrade, got {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
}

/// A real 101 Switching Protocols handshake needs a live hyper connection
/// (`hyper::upgrade::OnUpgrade` only exists when a connection can actually be
/// upgraded), so this test serves the router on an ephemeral port and drives it
/// with a real WebSocket client.
async fn serve(app: axum::Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

#[tokio::test]
async fn ws_upgrade_with_token_is_accepted_and_echoes_ping() {
    let (app, key) = test_app();
    let addr = serve(app).await;
    let url = format!("ws://{addr}/ws?token={}", access_token(&key, "7"));

    let (mut socket, response) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("handshake with a valid token must succeed");
    assert_eq!(
        response.status(),
        StatusCode::SWITCHING_PROTOCOLS,
        "a valid upgrade with a token must return 101"
    );

    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"action":"ping"}"#.into(),
        ))
        .await
        .expect("ping sends");

    let reply = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("server answers within 5s")
        .expect("a message arrives")
        .expect("message is not a protocol error");

    assert_eq!(
        reply.into_text().expect("text frame").as_str(),
        r#"{"action":"pong"}"#,
        "the server must answer ping with pong"
    );

    socket.close(None).await.expect("close cleanly");
}

#[tokio::test]
async fn ws_upgrade_without_token_is_refused() {
    let (app, _key) = test_app();
    let addr = serve(app).await;
    let url = format!("ws://{addr}/ws?token=");
    let status = handshake_status(&url)
        .await
        .expect("server sent an HTTP response");
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a handshake without a token must be refused with 401"
    );
}

/// Performs a WebSocket handshake and returns the HTTP status the server
/// replied with, for both accepted (101) and rejected handshakes.
async fn handshake_status(url: &str) -> Option<StatusCode> {
    match tokio_tungstenite::connect_async(url).await {
        Ok((mut socket, response)) => {
            let status = Some(response.status());
            let _ = socket.close(None).await;
            status
        }
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => Some(response.status()),
        Err(_) => None,
    }
}

#[tokio::test]
async fn ws_connection_limit_is_enforced() {
    let (app, key) = test_app();
    let addr = serve(app).await; // test_config() sets max_connections = 2
    let token = access_token(&key, "1");

    let mut sockets = Vec::new();
    for _ in 0..2 {
        let (socket, response) =
            tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={token}"))
                .await
                .expect("the first two connections are accepted");
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        sockets.push(socket);
    }

    let status = handshake_status(&format!("ws://{addr}/ws?token={token}"))
        .await
        .expect("server sent an HTTP response");
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "exceeding max_connections must yield 503"
    );

    for mut socket in sockets {
        let _ = socket.close(None).await;
    }
}

#[tokio::test]
async fn unknown_route_is_not_found() {
    let (app, _key) = test_app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/definitely-not-a-route")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}


#[tokio::test]
async fn ws_upgrade_with_forged_token_is_refused() {
    // The old handler accepted `?token=user_<id>` verbatim, so anyone could
    // subscribe to another user's private topics. Strings of that shape must now
    // fail signature verification.
    let (app, _key) = test_app();
    let addr = serve(app).await;

    let status = handshake_status(&format!("ws://{addr}/ws?token=user_7"))
        .await
        .expect("server sent an HTTP response");
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a token without a valid signature must be refused"
    );
}

#[tokio::test]
async fn ws_upgrade_with_token_signed_by_another_key_is_refused() {
    let (app, _key) = test_app();
    let addr = serve(app).await;
    let attacker = test_key_pair();
    let forged = access_token(&attacker, "7");

    let status = handshake_status(&format!("ws://{addr}/ws?token={forged}"))
        .await
        .expect("server sent an HTTP response");
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a token signed by an unknown key must be refused"
    );
}

#[tokio::test]
async fn ws_upgrade_without_a_configured_key_fails_closed() {
    // No verification key configured must mean 503, never "trust the client".
    let app = test_app_with("");
    let addr = serve(app).await;
    let key = test_key_pair();
    let token = access_token(&key, "7");

    let status = handshake_status(&format!("ws://{addr}/ws?token={token}"))
        .await
        .expect("server sent an HTTP response");
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "an unconfigured verification key must refuse handshakes"
    );
}

#[tokio::test]
async fn ws_upgrade_accepts_the_session_cookie() {
    // Browsers cannot set an Authorization header on a WebSocket handshake, so
    // the HttpOnly access_token cookie is the path the web app actually uses.
    let (app, key) = test_app();
    let addr = serve(app).await;
    let token = access_token(&key, "7");

    let request = tungstenite::handshake::client::Request::builder()
        .uri(format!("ws://{addr}/ws"))
        .header("Host", addr.to_string())
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .header("Cookie", format!("access_token={token}"))
        .body(())
        .expect("handshake request builds");

    let (mut socket, response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("handshake with a session cookie must succeed");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    let _ = socket.close(None).await;
}

#[test]
fn topic_requests_parse_into_known_topics() {
    let cases = [
        ("event_odds", 1_i64, Topic::EventOdds(1)),
        ("event_stats", 2, Topic::EventStats(2)),
        ("sport_scores", 3, Topic::SportScores(3)),
        ("user_bets", 4, Topic::UserBets(4)),
        ("user_balance", 5, Topic::UserBalance(5)),
    ];

    for (topic_type, id, expected) in cases {
        let req = TopicRequest {
            topic_type: topic_type.into(),
            id,
        };
        assert_eq!(parse_topic(&req), Some(expected), "{topic_type} must parse");
    }

    let unknown = TopicRequest {
        topic_type: "not_a_topic".into(),
        id: 9,
    };
    assert_eq!(
        parse_topic(&unknown),
        None,
        "unknown topic types must be rejected"
    );
}

#[test]
fn kafka_events_map_to_broadcast_topics() {
    // User-scoped Kafka topics are keyed by user id.
    for kafka_topic in ["bets.bet.placed", "bets.bet.settled", "bets.bet.cashout"] {
        assert_eq!(
            kafka_to_topic(kafka_topic, "42"),
            Some(Topic::UserBets(42)),
            "{kafka_topic} must route to the user's bet stream"
        );
    }

    assert_eq!(
        kafka_to_topic("events.odds_updated", "7"),
        Some(Topic::EventOdds(7))
    );
    assert_eq!(
        kafka_to_topic("analytics.events", "7"),
        Some(Topic::EventStats(7))
    );

    // Non-numeric keys and unsubscribed topics must not produce a Topic.
    assert_eq!(kafka_to_topic("events.odds_updated", "not-a-number"), None);
    assert_eq!(kafka_to_topic("some.unrelated.topic", "1"), None);
}

#[test]
fn subscription_manager_accounts_for_topics() {
    let mgr = SubscriptionManager::new();
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let topic = Topic::EventOdds(1);

    assert_eq!(mgr.subscriber_count(&topic), 0);
    assert_eq!(mgr.total_subscriptions(), 0);

    mgr.subscribe(topic.clone(), tx.clone());
    mgr.subscribe(topic.clone(), tx);
    assert_eq!(mgr.subscriber_count(&topic), 2);
    assert_eq!(mgr.total_subscriptions(), 2);

    // Unsubscribing removes exactly the given sender.
    let (other_tx, _other_rx) = tokio::sync::mpsc::channel(4);
    mgr.subscribe(topic.clone(), other_tx.clone());
    assert_eq!(mgr.subscriber_count(&topic), 3);

    let (to_remove, _to_remove_rx) = tokio::sync::mpsc::channel(4);
    mgr.subscribe(topic.clone(), to_remove.clone());
    mgr.unsubscribe(&topic, &to_remove);
    assert_eq!(
        mgr.subscriber_count(&topic),
        3,
        "unsubscribe must only drop the matching sender"
    );
}

#[tokio::test]
async fn closed_subscribers_are_reaped_on_broadcast() {
    let mgr = SubscriptionManager::new();
    let topic = Topic::EventOdds(1);

    let (dropped_tx, dropped_rx) = tokio::sync::mpsc::channel(4);
    let (live_tx, mut live_rx) = tokio::sync::mpsc::channel(4);
    drop(dropped_rx);

    mgr.subscribe(topic.clone(), dropped_tx);
    mgr.subscribe(topic.clone(), live_tx);

    mgr.broadcast(&topic, b"payload");

    assert_eq!(
        mgr.subscriber_count(&topic),
        1,
        "closed senders must be dropped during broadcast"
    );
    assert_eq!(
        live_rx.recv().await,
        Some(axum::extract::ws::Message::Binary(b"payload".to_vec())),
        "the live subscriber must receive the broadcast payload"
    );
}

#[tokio::test]
async fn connection_count_tracks_increment_and_decrement() {
    let mgr = SubscriptionManager::new();
    assert_eq!(mgr.connection_count(), 0);

    mgr.increment_connections();
    mgr.increment_connections();
    assert_eq!(mgr.connection_count(), 2);

    mgr.decrement_connections();
    assert_eq!(mgr.connection_count(), 1);
}

#[test]
fn client_messages_use_action_tagged_json() {
    let subscribe: ClientMessage =
        serde_json::from_str(r#"{"action":"subscribe","topics":[{"type":"event_odds","id":1}]}"#)
            .expect("subscribe envelope deserializes");
    match subscribe {
        ClientMessage::Subscribe { topics } => {
            assert_eq!(topics.len(), 1);
            assert_eq!(topics[0].topic_type, "event_odds");
            assert_eq!(topics[0].id, 1);
        }
        other => panic!("expected Subscribe, got {other:?}"),
    }

    let ping: ClientMessage =
        serde_json::from_str(r#"{"action":"ping"}"#).expect("ping envelope deserializes");
    assert!(matches!(ping, ClientMessage::Ping));

    assert!(
        serde_json::from_str::<ClientMessage>(r#"{"action":"nope"}"#).is_err(),
        "unknown actions must be rejected"
    );
}

#[test]
fn subscribed_kafka_topics_are_declared() {
    assert!(
        KAFKA_TOPICS.contains(&"events.odds_updated"),
        "odds updates must be subscribed"
    );
    assert!(
        KAFKA_TOPICS.contains(&"bets.bet.placed"),
        "bet placements must be subscribed"
    );
    // Every subscribed topic must have a routing rule, otherwise messages are
    // silently dropped.
    for topic in KAFKA_TOPICS {
        assert!(
            kafka_to_topic(topic, "1").is_some(),
            "subscribed topic {topic} has no kafka_to_topic mapping"
        );
    }
}

// Load test scenarios (require k6 or similar)
//
// 100K concurrent connections:
//   - k6 script connecting 100K WS clients
//   - Subscribe to random event topics
//   - Measure latency from Redpanda to client delivery
//   - Target: p99 < 10ms, p50 < 5ms
//
// Message throughput:
//   - Send 1M messages/sec through Kafka
//   - Verify all subscribers receive messages
//   - Measure broadcast latency
//
// Reconnection:
//   - Disconnect 50% of clients
//   - Verify they can reconnect
//   - Verify subscriptions are re-established
