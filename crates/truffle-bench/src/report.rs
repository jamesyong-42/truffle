//! Bench result serialisation.
//!
//! Writes one JSON file per run to `{out_dir}/{timestamp}-{workload}.json`.

use std::path::PathBuf;

use serde::Serialize;

#[derive(Serialize)]
pub struct BenchReport<P: Serialize, R: Serialize> {
    pub workload: String,
    pub timestamp_utc: String,
    pub git_sha: Option<String>,
    pub params: P,
    pub results: R,
}

pub fn write<P: Serialize, R: Serialize>(
    out_dir: &str,
    workload: &str,
    params: P,
    results: R,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(out_dir)?;

    let now_utc = chrono::Utc::now();
    let stamp = now_utc.format("%Y%m%dT%H%M%SZ").to_string();
    let path = PathBuf::from(out_dir).join(format!("{stamp}-{workload}.json"));

    let report = BenchReport {
        workload: workload.to_string(),
        timestamp_utc: now_utc.to_rfc3339(),
        git_sha: current_git_sha(),
        params,
        results,
    };

    let json = serde_json::to_string_pretty(&report).expect("serialize bench report");
    std::fs::write(&path, json)?;
    Ok(path)
}

fn current_git_sha() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
