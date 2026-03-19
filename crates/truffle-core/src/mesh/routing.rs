use crate::protocol::envelope::{MeshEnvelope, MESH_NAMESPACE};

/// STAR topology routing decisions.
///
/// In STAR topology:
/// - Primary is the hub
/// - Secondaries connect only to primary
/// - Secondaries route messages to other secondaries through the primary
///
/// This module contains pure routing logic with no I/O.

/// Determines how a message should be sent to reach a target device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    /// Send directly to the target (we have a direct connection).
    Direct,
    /// Route through the primary (we're a secondary, no direct connection).
    ViaPrimary {
        primary_id: String,
    },
    /// We are the primary; forward to the target.
    ForwardAsHub,
    /// Target is ourselves; deliver locally.
    Local,
    /// Cannot route (no primary known, no direct connection).
    Unroutable,
}

/// Determine routing decision for a message to a target device.
pub fn route_to_device(
    local_device_id: &str,
    target_device_id: &str,
    is_primary: bool,
    primary_id: Option<&str>,
    has_direct_connection: bool,
) -> RouteDecision {
    if target_device_id == local_device_id {
        return RouteDecision::Local;
    }

    if has_direct_connection {
        return RouteDecision::Direct;
    }

    if !is_primary {
        // Secondary without direct connection: route through primary
        if let Some(pid) = primary_id {
            return RouteDecision::ViaPrimary {
                primary_id: pid.to_string(),
            };
        }
        return RouteDecision::Unroutable;
    }

    // We are primary but have no direct connection to target
    RouteDecision::Unroutable
}

/// Create a route-message envelope that wraps another envelope for routing via primary.
pub fn wrap_route_message(target_device_id: &str, envelope: &MeshEnvelope) -> Option<MeshEnvelope> {
    let inner_value = match serde_json::to_value(envelope) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("Failed to serialize inner envelope for route-message: {e}");
            return None;
        }
    };
    Some(MeshEnvelope::new(
        MESH_NAMESPACE,
        "route-message",
        serde_json::json!({
            "targetDeviceId": target_device_id,
            "envelope": inner_value
        }),
    ))
}

/// Create a route-broadcast envelope that wraps another envelope for broadcast via primary.
pub fn wrap_route_broadcast(envelope: &MeshEnvelope) -> Option<MeshEnvelope> {
    let inner_value = match serde_json::to_value(envelope) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("Failed to serialize inner envelope for route-broadcast: {e}");
            return None;
        }
    };
    Some(MeshEnvelope::new(
        MESH_NAMESPACE,
        "route-broadcast",
        serde_json::json!({
            "envelope": inner_value
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_to_self_is_local() {
        let decision = route_to_device("dev-1", "dev-1", false, None, false);
        assert_eq!(decision, RouteDecision::Local);
    }

    #[test]
    fn route_with_direct_connection() {
        let decision = route_to_device("dev-1", "dev-2", false, Some("dev-3"), true);
        assert_eq!(decision, RouteDecision::Direct);
    }

    #[test]
    fn secondary_routes_via_primary() {
        let decision = route_to_device("dev-1", "dev-2", false, Some("dev-3"), false);
        assert_eq!(
            decision,
            RouteDecision::ViaPrimary {
                primary_id: "dev-3".to_string()
            }
        );
    }

    #[test]
    fn secondary_no_primary_is_unroutable() {
        let decision = route_to_device("dev-1", "dev-2", false, None, false);
        assert_eq!(decision, RouteDecision::Unroutable);
    }

    #[test]
    fn primary_no_direct_connection_is_unroutable() {
        let decision = route_to_device("dev-1", "dev-2", true, Some("dev-1"), false);
        assert_eq!(decision, RouteDecision::Unroutable);
    }

    #[test]
    fn wrap_route_message_creates_envelope() {
        let inner = MeshEnvelope::new("sync", "update", serde_json::json!({"key": "val"}));
        let wrapped = wrap_route_message("dev-2", &inner)
            .expect("wrap_route_message must succeed for valid envelope");
        assert_eq!(wrapped.namespace, MESH_NAMESPACE);
        assert_eq!(wrapped.msg_type, "route-message");

        let payload = &wrapped.payload;
        assert_eq!(
            payload.get("targetDeviceId").unwrap().as_str().unwrap(),
            "dev-2"
        );
        assert!(payload.get("envelope").is_some());
    }

    #[test]
    fn wrap_route_broadcast_creates_envelope() {
        let inner = MeshEnvelope::new("sync", "full", serde_json::json!(null));
        let wrapped = wrap_route_broadcast(&inner)
            .expect("wrap_route_broadcast must succeed for valid envelope");
        assert_eq!(wrapped.namespace, MESH_NAMESPACE);
        assert_eq!(wrapped.msg_type, "route-broadcast");
        assert!(wrapped.payload.get("envelope").is_some());
    }
}
