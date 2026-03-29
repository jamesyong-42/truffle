use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=TRUFFLE_SIDECAR_PATH");
    println!("cargo:rerun-if-env-changed=TRUFFLE_SIDECAR_SKIP_DOWNLOAD");

    // If user provides their own sidecar, skip download
    if let Ok(path) = env::var("TRUFFLE_SIDECAR_PATH") {
        println!("cargo:rustc-env=TRUFFLE_SIDECAR_PATH={path}");
        return;
    }

    // Allow skipping download (e.g., for CI where sidecar is provided separately)
    if env::var("TRUFFLE_SIDECAR_SKIP_DOWNLOAD").is_ok() {
        println!("cargo:rustc-env=TRUFFLE_SIDECAR_PATH=truffle-sidecar");
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    let version = env!("CARGO_PKG_VERSION");

    let (asset_name, bin_name) = match target.as_str() {
        "aarch64-apple-darwin" => ("tsnet-sidecar-darwin-arm64", "sidecar-slim"),
        "x86_64-apple-darwin" => ("tsnet-sidecar-darwin-amd64", "sidecar-slim"),
        "x86_64-unknown-linux-gnu" => ("tsnet-sidecar-linux-amd64", "sidecar-slim"),
        "aarch64-unknown-linux-gnu" => ("tsnet-sidecar-linux-arm64", "sidecar-slim"),
        "x86_64-pc-windows-msvc" => ("tsnet-sidecar-windows-amd64.exe", "sidecar-slim.exe"),
        other => {
            eprintln!("cargo:warning=truffle-sidecar: unsupported target {other}, skipping download");
            println!("cargo:rustc-env=TRUFFLE_SIDECAR_PATH=truffle-sidecar");
            return;
        }
    };

    let sidecar_path = out_dir.join(bin_name);

    // Skip download if already present (cached build)
    if sidecar_path.exists() {
        println!(
            "cargo:rustc-env=TRUFFLE_SIDECAR_PATH={}",
            sidecar_path.display()
        );
        return;
    }

    let url = format!(
        "https://github.com/jamesyong-42/truffle/releases/download/truffle-v{version}/{asset_name}"
    );

    eprintln!("truffle-sidecar: downloading {url}");

    match download(&url, &sidecar_path) {
        Ok(()) => {
            #[cfg(unix)]
            set_executable(&sidecar_path);

            println!(
                "cargo:rustc-env=TRUFFLE_SIDECAR_PATH={}",
                sidecar_path.display()
            );
            eprintln!(
                "truffle-sidecar: downloaded to {}",
                sidecar_path.display()
            );
        }
        Err(e) => {
            eprintln!("cargo:warning=truffle-sidecar: failed to download sidecar: {e}");
            eprintln!("cargo:warning=truffle-sidecar: set TRUFFLE_SIDECAR_PATH to provide it manually");
            // Fall back to PATH lookup at runtime
            println!("cargo:rustc-env=TRUFFLE_SIDECAR_PATH=truffle-sidecar");
        }
    }
}

fn download(url: &str, dest: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let response = ureq::get(url).call()?;

    let status = response.status();
    if status != 200 {
        return Err(format!("HTTP {status} from {url}").into());
    }

    let mut reader = response.into_body().into_reader();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut bytes)?;

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dest, &bytes)?;

    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = fs::metadata(path) {
        let mut perms = metadata.permissions();
        perms.set_mode(0o755);
        let _ = fs::set_permissions(path, perms);
    }
}
