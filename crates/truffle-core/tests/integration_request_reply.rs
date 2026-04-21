//! Integration tests for the request/reply utility over a real tailnet pair.
//!
//! Closes the Layer-6 gap in RFC 019 §1: `send_and_wait` is only tested
//! against MockNetworkProvider elsewhere.
//!
//! Skipped when `TRUFFLE_TEST_AUTHKEY` is not set.

mod common;

use std::time::Duration;

use serde_json::json;
use truffle_core::request_reply::{send_and_wait, RequestError};

const RR_TIMEOUT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Test 1: Basic request/reply round trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_request_reply_basic_roundtrip() {
    let Some(authkey) = common::require_authkey("test_request_reply_basic_roundtrip") else {
        return;
    };
    common::init_test_tracing();

    let pair = common::make_truffle_pair(&authkey).await;

    // Beta: minimal responder — on any "ping", send a "pong" back.
    let beta = pair.beta.clone();
    let responder = tokio::spawn(async move {
        let mut rx = beta.subscribe("echo");
        while let Ok(msg) = rx.recv().await {
            if msg.msg_type == "ping" {
                let reply = json!({ "echoed": msg.payload });
                let _ = beta.send_typed(&msg.from, "echo", "pong", &reply).await;
            }
        }
    });

    let payload = json!({ "hello": "world" });
    let reply = send_and_wait(
        &pair.alpha,
        &pair.beta_device_id,
        "echo",
        "ping",
        &payload,
        RR_TIMEOUT,
        |msg| {
            if msg.msg_type == "pong" {
                Some(msg.payload.clone())
            } else {
                None
            }
        },
    )
    .await
    .expect("request/reply roundtrip should succeed");

    assert_eq!(reply, json!({ "echoed": { "hello": "world" } }));

    responder.abort();
    pair.stop().await;
}

// ---------------------------------------------------------------------------
// Test 2: Large payload round trip — 128KB of JSON string
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_request_reply_large_payload() {
    let Some(authkey) = common::require_authkey("test_request_reply_large_payload") else {
        return;
    };
    common::init_test_tracing();

    let pair = common::make_truffle_pair(&authkey).await;

    // Beta echoes whatever it gets on "big".
    let beta = pair.beta.clone();
    let responder = tokio::spawn(async move {
        let mut rx = beta.subscribe("big");
        while let Ok(msg) = rx.recv().await {
            if msg.msg_type == "req" {
                let _ = beta
                    .send_typed(&msg.from, "big", "resp", &msg.payload)
                    .await;
            }
        }
    });

    let big = "x".repeat(128 * 1024);
    let payload = json!({ "data": big });

    let reply = send_and_wait(
        &pair.alpha,
        &pair.beta_device_id,
        "big",
        "req",
        &payload,
        Duration::from_secs(30),
        |msg| {
            if msg.msg_type == "resp" {
                Some(msg.payload.clone())
            } else {
                None
            }
        },
    )
    .await
    .expect("large request/reply should succeed");

    assert_eq!(
        reply, payload,
        "reply payload must match request byte-for-byte"
    );

    responder.abort();
    pair.stop().await;
}

// ---------------------------------------------------------------------------
// Test 3: Timeout when peer doesn't respond
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_request_reply_timeout_when_no_responder() {
    let Some(authkey) = common::require_authkey("test_request_reply_timeout_when_no_responder")
    else {
        return;
    };
    common::init_test_tracing();

    let pair = common::make_truffle_pair(&authkey).await;

    // No responder on beta — alpha should time out.
    let result = send_and_wait(
        &pair.alpha,
        &pair.beta_device_id,
        "silence",
        "hello",
        &json!({}),
        Duration::from_secs(2),
        |_msg| Some(()),
    )
    .await;

    assert!(
        matches!(result, Err(RequestError::Timeout)),
        "expected RequestError::Timeout, got {result:?}"
    );

    pair.stop().await;
}
