//! `truffle uninstall` -- remove truffle from this machine.
//!
//! Follows industry conventions (rustup self uninstall, claude uninstall):
//! 1. Stop the daemon if running
//! 2. Remove binaries (truffle + sidecar)
//! 3. Remove PATH entry from shell profile
//! 4. Optionally remove config and state
//! 5. Print confirmation

use crate::output;

pub async fn run(keep_config: bool) -> Result<(), String> {
    println!();
    println!("  {}", output::bold("truffle uninstall"));
    println!("  {}", output::dim(&"\u{2500}".repeat(39)));
    println!();

    // 1. Stop daemon if running
    let client = crate::daemon::client::DaemonClient::new();
    if client.is_daemon_running() {
        println!("  Stopping daemon...");
        if let Ok(_) = client
            .request(
                crate::daemon::protocol::method::SHUTDOWN,
                serde_json::json!({}),
            )
            .await
        {
            println!("  {} Daemon stopped", output::green("\u{2713}"));
        }
        // Give it a moment to clean up
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }

    // 2. Find and remove binaries
    let install_dir = dirs::config_dir()
        .map(|d| d.join("truffle").join("bin"))
        .unwrap_or_default();

    let exe_path = std::env::current_exe().ok();

    // Remove sidecar from install dir
    for name in &["sidecar-slim", "truffle-sidecar"] {
        let path = install_dir.join(name);
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }
        #[cfg(windows)]
        {
            let exe_name = format!("{}.exe", name);
            let path = install_dir.join(&exe_name);
            if path.exists() {
                std::fs::remove_file(&path).ok();
            }
        }
    }
    println!("  {} Sidecar removed", output::green("\u{2713}"));

    // 3. Remove PID file and socket
    let config_dir = dirs::config_dir()
        .map(|d| d.join("truffle"))
        .unwrap_or_default();

    let _ = std::fs::remove_file(config_dir.join("truffle.pid"));
    #[cfg(unix)]
    let _ = std::fs::remove_file(config_dir.join("truffle.sock"));

    // 4. Optionally remove config and state
    if !keep_config {
        if config_dir.join("state").exists() {
            std::fs::remove_dir_all(config_dir.join("state")).ok();
            println!("  {} Tailscale state removed", output::green("\u{2713}"));
        }
        if config_dir.join("config.toml").exists() {
            std::fs::remove_file(config_dir.join("config.toml")).ok();
            println!("  {} Config removed", output::green("\u{2713}"));
        }
    } else {
        println!("  {} Config and state kept (use --purge to remove)", output::dim("\u{2022}"));
    }

    // Remove empty dirs
    let _ = std::fs::remove_dir(install_dir);
    if !keep_config {
        let _ = std::fs::remove_dir(&config_dir);
    }

    // 5. Clean up shell profile
    #[cfg(unix)]
    {
        let shell = std::env::var("SHELL").unwrap_or_default();
        let shell_name = std::path::Path::new(&shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("sh");

        let profile = match shell_name {
            "zsh" => dirs::home_dir().map(|h| h.join(".zshrc")),
            "bash" => {
                let bashrc = dirs::home_dir().map(|h| h.join(".bashrc"));
                if bashrc.as_ref().map_or(false, |p| p.exists()) {
                    bashrc
                } else {
                    dirs::home_dir().map(|h| h.join(".bash_profile"))
                }
            }
            _ => dirs::home_dir().map(|h| h.join(".profile")),
        };

        if let Some(profile_path) = profile {
            if profile_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&profile_path) {
                    if content.contains("# Added by truffle installer") {
                        let cleaned: Vec<&str> = content
                            .lines()
                            .filter(|line| {
                                !line.contains("# Added by truffle installer")
                                    && !line.contains(".config/truffle/bin")
                            })
                            .collect();
                        std::fs::write(&profile_path, cleaned.join("\n") + "\n").ok();
                        println!(
                            "  {} PATH removed from {}",
                            output::green("\u{2713}"),
                            profile_path.display()
                        );
                    }
                }
            }
        }
    }

    // 6. Remove the truffle binary itself (schedule for after exit)
    if let Some(exe) = exe_path {
        println!();
        println!(
            "  {} truffle uninstalled",
            output::green("\u{2713}")
        );
        println!();

        // On Unix, we can delete the running binary
        #[cfg(unix)]
        {
            std::fs::remove_file(&exe).ok();
            println!("  Binary removed: {}", output::dim(&exe.display().to_string()));
        }

        // On Windows, running binary can't be deleted — print instruction
        #[cfg(windows)]
        {
            println!(
                "  Please delete the binary manually: {}",
                exe.display()
            );
        }
    } else {
        println!();
        println!(
            "  {} truffle uninstalled",
            output::green("\u{2713}")
        );
    }

    println!();
    println!("  Open a new terminal for PATH changes to take effect.");
    println!();

    Ok(())
}
