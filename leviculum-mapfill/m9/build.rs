//! Stamps the image with the optimisation level it was built at and the
//! commit, so a UART capture names the build it measured.

use std::process::Command;

fn main() {
    // Cargo hands build scripts the profile's opt-level: "3" for
    // `--release`, "s" for `--profile release-s`.
    let opt = std::env::var("OPT_LEVEL").unwrap_or_else(|_| "unknown".to_string());
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=MAPFILL_OPT={opt}");
    println!("cargo:rustc-env=MAPFILL_SHA={sha}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../src");
    println!("cargo:rerun-if-changed=../fixtures");
}
