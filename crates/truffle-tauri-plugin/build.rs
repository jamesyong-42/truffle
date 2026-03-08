const COMMANDS: &[&str] = &[
    "start",
    "stop",
    "is_running",
    "device_id",
    "devices",
    "device_by_id",
    "is_primary",
    "primary_id",
    "role",
    "send_envelope",
    "broadcast_envelope",
    "handle_tailnet_peers",
    "set_local_online",
    "proxy_add",
    "proxy_remove",
    "proxy_list",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
