use std::sync::Arc;

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Runtime};
use tokio::sync::RwLock;

use truffle_core::mesh::node::MeshNode;
use truffle_core::reverse_proxy::ProxyManager;

pub mod commands;
pub mod events;

/// Shared state managed by the Tauri plugin.
///
/// The MeshNode and ProxyManager are wrapped in RwLock<Option<>> because
/// they are created lazily after the app starts (e.g., after reading config).
/// The Tauri plugin manages their lifecycle.
pub struct TruffleState {
    pub mesh_node: RwLock<Option<Arc<MeshNode>>>,
    pub proxy_manager: RwLock<Option<Arc<ProxyManager>>>,
}

impl Default for TruffleState {
    fn default() -> Self {
        Self {
            mesh_node: RwLock::new(None),
            proxy_manager: RwLock::new(None),
        }
    }
}

/// Initialize the Truffle Tauri v2 plugin.
///
/// Usage in Tauri app:
/// ```ignore
/// fn main() {
///     tauri::Builder::default()
///         .plugin(truffle_tauri_plugin::init())
///         .run(tauri::generate_context!())
///         .expect("error while running tauri application");
/// }
/// ```
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("truffle")
        .invoke_handler(tauri::generate_handler![
            commands::start,
            commands::stop,
            commands::is_running,
            commands::device_id,
            commands::devices,
            commands::device_by_id,
            commands::is_primary,
            commands::primary_id,
            commands::role,
            commands::send_envelope,
            commands::broadcast_envelope,
            commands::handle_tailnet_peers,
            commands::set_local_online,
            commands::proxy_add,
            commands::proxy_remove,
            commands::proxy_list,
        ])
        .setup(|app, _api| {
            app.manage(TruffleState::default());
            Ok(())
        })
        .build()
}
