use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};

use crate::protocol::envelope::MeshEnvelope;

/// A message received from the bus.
#[derive(Debug, Clone)]
pub struct BusMessage {
    /// Source identifier (deviceId for mesh).
    pub from: Option<String>,
    /// Message namespace.
    pub namespace: String,
    /// Message type within namespace.
    pub msg_type: String,
    /// Message payload.
    pub payload: serde_json::Value,
}

/// Events from the MessageBus.
#[derive(Debug, Clone)]
pub enum MessageBusEvent {
    Subscribed(String),
    Unsubscribed(String),
    Error { error: String, namespace: String },
}

/// Trait for the underlying mesh node that the bus uses to send messages.
pub trait MeshTransport: Send + Sync {
    /// Send an envelope to a specific device.
    fn send_envelope(&self, device_id: &str, envelope: &MeshEnvelope) -> bool;
    /// Broadcast an envelope to all connected devices.
    fn broadcast_envelope(&self, envelope: &MeshEnvelope);
    /// Get the local device ID.
    fn local_device_id(&self) -> &str;
    /// Check if the mesh is running.
    fn is_running(&self) -> bool;
}

/// Callback type for bus message handlers.
pub type BusMessageHandler = Arc<dyn Fn(&BusMessage) + Send + Sync>;

/// MeshMessageBus - Application-facing pub/sub layer for mesh messaging.
///
/// Implements namespace-based subscription and publishing.
/// Layer 2 in the messaging architecture.
pub struct MeshMessageBus {
    handlers: Arc<RwLock<HashMap<String, Vec<BusMessageHandler>>>>,
    event_tx: mpsc::Sender<MessageBusEvent>,
}

impl MeshMessageBus {
    pub fn new(event_tx: mpsc::Sender<MessageBusEvent>) -> Self {
        Self {
            handlers: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
        }
    }

    /// Subscribe to messages in a namespace.
    /// Returns a subscription ID that can be used to unsubscribe.
    pub async fn subscribe(
        &self,
        namespace: &str,
        handler: BusMessageHandler,
    ) -> SubscriptionId {
        let mut handlers = self.handlers.write().await;
        let entry = handlers.entry(namespace.to_string()).or_default();
        let id = SubscriptionId {
            namespace: namespace.to_string(),
            index: entry.len(),
        };
        entry.push(handler);

        let _ = self.event_tx.try_send(MessageBusEvent::Subscribed(namespace.to_string()));
        id
    }

    /// Dispatch an incoming message to all handlers for its namespace.
    pub async fn dispatch(&self, message: &BusMessage) {
        let handlers = self.handlers.read().await;
        if let Some(namespace_handlers) = handlers.get(&message.namespace) {
            for handler in namespace_handlers {
                if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    handler(message);
                })) {
                    let err_msg = format!("Handler panicked: {e:?}");
                    tracing::error!("MessageBus handler error in namespace '{}': {err_msg}", message.namespace);
                    let _ = self.event_tx.try_send(MessageBusEvent::Error {
                        error: err_msg,
                        namespace: message.namespace.clone(),
                    });
                }
            }
        }
    }

    /// Get list of subscribed namespaces.
    pub async fn subscribed_namespaces(&self) -> Vec<String> {
        let handlers = self.handlers.read().await;
        handlers.keys().filter(|k| {
            handlers.get(*k).map_or(false, |v| !v.is_empty())
        }).cloned().collect()
    }

    /// Remove all handlers and clean up.
    pub async fn dispose(&self) {
        let mut handlers = self.handlers.write().await;
        handlers.clear();
    }
}

/// Identifies a subscription for potential unsubscription.
#[derive(Debug, Clone)]
pub struct SubscriptionId {
    pub namespace: String,
    pub index: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn subscribe_and_dispatch() {
        let (tx, _rx) = mpsc::channel(64);
        let bus = MeshMessageBus::new(tx);

        let call_count = Arc::new(AtomicUsize::new(0));
        let count_clone = call_count.clone();

        bus.subscribe("test-ns", Arc::new(move |msg: &BusMessage| {
            assert_eq!(msg.namespace, "test-ns");
            assert_eq!(msg.msg_type, "hello");
            count_clone.fetch_add(1, Ordering::SeqCst);
        })).await;

        let message = BusMessage {
            from: Some("device-1".to_string()),
            namespace: "test-ns".to_string(),
            msg_type: "hello".to_string(),
            payload: serde_json::json!({"key": "val"}),
        };

        bus.dispatch(&message).await;
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatch_to_wrong_namespace_does_nothing() {
        let (tx, _rx) = mpsc::channel(64);
        let bus = MeshMessageBus::new(tx);

        let call_count = Arc::new(AtomicUsize::new(0));
        let count_clone = call_count.clone();

        bus.subscribe("ns-a", Arc::new(move |_msg: &BusMessage| {
            count_clone.fetch_add(1, Ordering::SeqCst);
        })).await;

        let message = BusMessage {
            from: None,
            namespace: "ns-b".to_string(),
            msg_type: "test".to_string(),
            payload: serde_json::json!(null),
        };

        bus.dispatch(&message).await;
        assert_eq!(call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn subscribed_namespaces() {
        let (tx, _rx) = mpsc::channel(64);
        let bus = MeshMessageBus::new(tx);

        bus.subscribe("alpha", Arc::new(|_| {})).await;
        bus.subscribe("beta", Arc::new(|_| {})).await;

        let ns = bus.subscribed_namespaces().await;
        assert!(ns.contains(&"alpha".to_string()));
        assert!(ns.contains(&"beta".to_string()));
    }

    #[tokio::test]
    async fn dispose_clears_handlers() {
        let (tx, _rx) = mpsc::channel(64);
        let bus = MeshMessageBus::new(tx);

        bus.subscribe("test", Arc::new(|_| {})).await;
        assert!(!bus.subscribed_namespaces().await.is_empty());

        bus.dispose().await;
        assert!(bus.subscribed_namespaces().await.is_empty());
    }

    #[tokio::test]
    async fn handler_panic_does_not_crash() {
        let (tx, _rx) = mpsc::channel(64);
        let bus = MeshMessageBus::new(tx);

        bus.subscribe("panic-ns", Arc::new(|_: &BusMessage| {
            panic!("test panic");
        })).await;

        let message = BusMessage {
            from: None,
            namespace: "panic-ns".to_string(),
            msg_type: "test".to_string(),
            payload: serde_json::json!(null),
        };

        // Should not panic
        bus.dispatch(&message).await;
    }
}
