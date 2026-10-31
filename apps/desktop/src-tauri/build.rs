mod build_credentials;
mod build_metadata;

fn main() {
    for (cargo, compiled) in [
        ("PROFILE", "PCL_BUILD_PROFILE"),
        ("TARGET", "PCL_BUILD_TARGET"),
    ] {
        let value = std::env::var(cargo).expect("Cargo build identity");
        println!("cargo:rustc-env={compiled}={value}");
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    build_credentials::generate(&out).unwrap_or_else(|error| panic!("{error}"));
    // Only a clean source build can truthfully bind the executable to a commit.
    // Source archives without Git still build, but same-version update ordering
    // remains unavailable rather than guessing identity from a later checkout.
    let project = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    // Frontend assets and legal documents also belong to this build identity.
    // Tauri's build helper does not track this custom frontendDist for us.
    for reference in [
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
    let commit = build_metadata::identity(&project).unwrap_or_default();
    println!("cargo:rustc-env=PCL_BUILD_COMMIT={commit}");
    tauri_build::build();
}
