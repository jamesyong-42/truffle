use serde::{Deserialize, Serialize};
use std::fmt;

/// Message namespace for the v3 wire protocol.
///
/// Known namespaces get compile-time checking. The `Custom` variant is an
/// extensibility escape hatch for application-defined pub/sub topics.
///
/// Wire representation (JSON strings): `"mesh"`, `"sync"`, `"file-transfer"`,
/// or any other string for `Custom`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Namespace {
    /// Internal mesh protocol: device discovery, peer management.
    Mesh,
    /// Store synchronization.
    Sync,
    /// File transfer signaling.
    FileTransfer,
    /// Application-defined namespace (extensibility escape hatch).
    ///
    /// Any string that does not match a known variant deserializes as `Custom`.
    #[serde(untagged)]
    Custom(String),
}

impl Namespace {
    /// Returns the wire-format string for this namespace.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Mesh => "mesh",
            Self::Sync => "sync",
            Self::FileTransfer => "file-transfer",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Serde roundtrip tests --

    #[test]
    fn namespace_mesh_serde_roundtrip() {
        let ns = Namespace::Mesh;
        let json = serde_json::to_string(&ns).unwrap();
        assert_eq!(json, r#""mesh""#);
        let parsed: Namespace = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Namespace::Mesh);
    }

    #[test]
    fn namespace_sync_serde_roundtrip() {
        let ns = Namespace::Sync;
        let json = serde_json::to_string(&ns).unwrap();
        assert_eq!(json, r#""sync""#);
        let parsed: Namespace = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Namespace::Sync);
    }

    #[test]
    fn namespace_file_transfer_serde_roundtrip() {
        let ns = Namespace::FileTransfer;
        let json = serde_json::to_string(&ns).unwrap();
        assert_eq!(json, r#""file-transfer""#);
        let parsed: Namespace = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Namespace::FileTransfer);
    }

    #[test]
    fn namespace_custom_serde_roundtrip() {
        let ns = Namespace::Custom("my-app".to_string());
        let json = serde_json::to_string(&ns).unwrap();
        assert_eq!(json, r#""my-app""#);
        let parsed: Namespace = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Namespace::Custom("my-app".to_string()));
    }

    #[test]
    fn namespace_custom_unknown_string_deserializes() {
        // Any string not matching a known variant becomes Custom.
        let parsed: Namespace = serde_json::from_str(r#""clipboard""#).unwrap();
        assert_eq!(parsed, Namespace::Custom("clipboard".to_string()));
    }

    #[test]
    fn namespace_custom_empty_string() {
        let parsed: Namespace = serde_json::from_str(r#""""#).unwrap();
        assert_eq!(parsed, Namespace::Custom("".to_string()));
    }

    // -- as_str tests --

    #[test]
    fn namespace_as_str() {
        assert_eq!(Namespace::Mesh.as_str(), "mesh");
        assert_eq!(Namespace::Sync.as_str(), "sync");
        assert_eq!(Namespace::FileTransfer.as_str(), "file-transfer");
        assert_eq!(
            Namespace::Custom("notifications".to_string()).as_str(),
            "notifications"
        );
    }

    // -- Display tests --

    #[test]
    fn namespace_display() {
        assert_eq!(format!("{}", Namespace::Mesh), "mesh");
        assert_eq!(format!("{}", Namespace::Sync), "sync");
        assert_eq!(format!("{}", Namespace::FileTransfer), "file-transfer");
        assert_eq!(
            format!("{}", Namespace::Custom("chat".to_string())),
            "chat"
        );
    }

    // -- Equality tests --

    #[test]
    fn namespace_equality() {
        assert_eq!(Namespace::Mesh, Namespace::Mesh);
        assert_ne!(Namespace::Mesh, Namespace::Sync);
        assert_ne!(Namespace::Mesh, Namespace::Custom("mesh".to_string()));
        assert_eq!(
            Namespace::Custom("x".to_string()),
            Namespace::Custom("x".to_string())
        );
        assert_ne!(
            Namespace::Custom("x".to_string()),
            Namespace::Custom("y".to_string())
        );
    }

    // -- Hash tests (Namespace is used as HashMap key) --

    #[test]
    fn namespace_hash_map_key() {
        use std::collections::HashMap;

        let mut map = HashMap::new();
        map.insert(Namespace::Mesh, "mesh handler");
        map.insert(Namespace::Sync, "sync handler");
        map.insert(Namespace::Custom("app".to_string()), "app handler");

        assert_eq!(map.get(&Namespace::Mesh), Some(&"mesh handler"));
        assert_eq!(map.get(&Namespace::Sync), Some(&"sync handler"));
        assert_eq!(
            map.get(&Namespace::Custom("app".to_string())),
            Some(&"app handler")
        );
        assert_eq!(map.get(&Namespace::FileTransfer), None);
    }

    // -- MessagePack roundtrip --

    #[test]
    fn namespace_msgpack_roundtrip() {
        let variants = [
            Namespace::Mesh,
            Namespace::Sync,
            Namespace::FileTransfer,
            Namespace::Custom("custom-ns".to_string()),
        ];
        for ns in &variants {
            let packed = rmp_serde::to_vec_named(ns).unwrap();
            let parsed: Namespace = rmp_serde::from_slice(&packed).unwrap();
            assert_eq!(&parsed, ns, "Msgpack roundtrip failed for {ns:?}");
        }
    }
}
