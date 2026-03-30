const COMMANDS: &[&str] = &[
    "start",
    "stop",
    "get_local_info",
    "get_peers",
    "ping",
    "health",
    "send_message",
    "broadcast",
    "send_file",
    "pull_file",
    "auto_accept",
    "auto_reject",
    "accept_offer",
    "reject_offer",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
