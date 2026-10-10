fn main() {
    // Only a clean source build can truthfully bind the executable to a commit.
    // Source archives without Git still build, but same-version update ordering
    // remains unavailable rather than guessing identity from a later checkout.
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(&project)
            .args(args)
            .output()
            .ok()
    };
    for reference in [".git/HEAD", ".git/refs/heads/master", ".git/packed-refs"] {
        println!(
            "cargo:rerun-if-changed={}",
            project.join(reference).display()
        );
    }
    let clean = git(&[
        "status",
        "--porcelain",
        "--untracked-files=normal",
        "--",
        "apps/desktop",
        "crates",
        "Cargo.toml",
        "Cargo.lock",
        "assets",
        "code",
        "experimental",
    ])
    .is_some_and(|result| result.status.success() && result.stdout.is_empty());
    let commit = if clean {
        git(&["rev-parse", "HEAD"])
            .filter(|r| r.status.success())
            .and_then(|r| String::from_utf8(r.stdout).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .unwrap_or_default()
    } else {
        String::new()
    };
    println!("cargo:rustc-env=PCL_BUILD_COMMIT={commit}");
    tauri_build::build();
}
