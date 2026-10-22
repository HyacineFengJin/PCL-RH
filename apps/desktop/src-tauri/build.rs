mod build_credentials;

fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    build_credentials::generate(&out).unwrap_or_else(|error| panic!("{error}"));
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
    // Frontend assets and legal documents also belong to this build identity.
    // Tauri's build helper does not track this custom frontendDist for us.
    for reference in [
        ".git/HEAD",
        ".git/refs/heads/master",
        ".git/packed-refs",
        "apps/desktop/dist",
        "LICENSE",
        "NOTICE",
        "COPYRIGHT.md",
        "THIRD_PARTY_NOTICES.md",
        "THIRDPARTY.generated.json",
        "LICENSES",
        "tools/licenses",
        "apps/desktop/src/assets/fonts/LICENSE",
        "apps/desktop/src/assets/fonts/NOTICE",
        "apps/desktop/src/assets/game-icons",
        "apps/desktop/src/assets/credits/NOTICE",
        "experimental/NOTICE.md",
        "experimental/runtime/vendor/maker/LICENSE",
        "experimental/runtime/vendor/porter/LICENSE",
    ] {
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
        "LICENSE",
        "NOTICE",
        "COPYRIGHT.md",
        "THIRD_PARTY_NOTICES.md",
        "THIRDPARTY.generated.json",
        "LICENSES",
        "tools/licenses",
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
