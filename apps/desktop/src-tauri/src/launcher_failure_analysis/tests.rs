use super::*;

fn codes(message: &str, log: &str) -> Vec<String> {
    analyze(message, Some(log))
        .into_iter()
        .map(|finding| finding.code)
        .collect()
}

#[test]
fn preparation_errors_point_to_java_or_missing_dependencies() {
    assert_eq!(
        codes("此实例需要 Java 21；手动选择的是 Java 17", ""),
        ["java_version"]
    );
    assert_eq!(
        codes("手动选择的 Java 不可用：Java 文件没有执行权限", ""),
        ["java_runtime"]
    );
    assert_eq!(
        codes(
            "Missing 12 installed dependencies:\n/libraries/example.jar",
            ""
        ),
        ["missing_dependency"]
    );
}

#[test]
fn each_specific_runtime_family_has_a_stable_hint() {
    for (line, code) in [
        (
            "java.lang.UnsupportedClassVersionError: Example",
            "java_version",
        ),
        ("java.lang.OutOfMemoryError: Java heap space", "java_memory"),
        ("Unrecognized VM option 'Example'", "java_option"),
        (
            "java.lang.ClassNotFoundException: example.Main",
            "missing_dependency",
        ),
        (
            "java.lang.UnsatisfiedLinkError: no lwjgl in java.library.path",
            "missing_dependency",
        ),
        (
            "Failed to initialize graphics window with current settings.",
            "graphics",
        ),
        (
            "GLFW error 65542: GLX: Failed to create context",
            "graphics",
        ),
        (
            "net.fabricmc.loader.impl.discovery.ModResolutionException: conflicting versions",
            "mod_loading",
        ),
        (
            "org.spongepowered.asm.mixin.throwables.MixinApplyError: could not apply",
            "mixin",
        ),
    ] {
        assert_eq!(codes("exited 1", line), [code]);
    }
}

#[test]
fn ordinary_environment_logs_and_signal_codes_do_not_diagnose_a_cause() {
    assert_eq!(codes("exited 137", "Java 21\nOpenGL version 4.6\nLoaded 300 mods\nGraphics initialized successfully\nA dependency check completed"), ["unknown"]);
    assert_eq!(
        codes(
            "exited 1",
            "java.lang.NoClassDefFoundError: Could not initialize class example.Registry"
        ),
        ["unknown"]
    );
}

#[test]
fn generic_vm_wrapper_does_not_distract_from_explicit_memory_error() {
    assert_eq!(
        codes(
            "exited 1",
            "Error: Could not create the Java Virtual Machine."
        ),
        ["unknown"]
    );
    assert_eq!(codes("exited 1", "Could not reserve enough space for object heap\nError: Could not create the Java Virtual Machine."), ["java_memory"]);
    assert_eq!(codes("exited 1", "Invalid maximum heap size\nUnrecognized VM option 'Other'\nError: Could not create the Java Virtual Machine."), ["java_memory", "java_option"]);
}

#[test]
fn evidence_is_deduplicated_and_bounded_without_breaking_unicode() {
    let line = format!(
        "java.lang.ClassNotFoundException: {}\u{0007}",
        "示例".repeat(400)
    );
    let log = format!("{line}\n{line}\njava.lang.ClassNotFoundException: second\njava.lang.ClassNotFoundException: third");
    let report = analyze("failed", Some(&log));
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].evidence.len(), 2);
    assert_eq!(report[0].evidence[0].chars().count(), MAX_EVIDENCE_CHARS);
    assert!(!report[0].evidence[0].chars().any(char::is_control));
}

#[test]
fn overlong_input_remains_bounded_and_returns_unknown_without_guessing() {
    let huge = "x".repeat(MAX_INPUT_BYTES + 1);
    assert_eq!(
        codes("failed", &format!("{huge}\nUnrecognized VM option: false")),
        ["unknown"]
    );
    assert_eq!(
        codes(
            "failed",
            &format!(
                "{}\nUnrecognized VM option: false",
                "normal\n".repeat(MAX_LINES)
            )
        ),
        ["unknown"]
    );
}

#[test]
fn neoforge_window_handoff_has_its_own_observed_stage() {
    let detail = "We seem to be having trouble handing off the window, tried for 1 second";
    let report = analyze(
        "exited 1",
        Some(&format!(
            "[main/ERROR] [EARLYDISPLAY/]: ERROR DISPLAY\n\
             Failed to initialize the mod loading system and display.\n\
             Failure details:\n{detail}"
        )),
    );
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].code, "graphics_handoff");
    assert_eq!(report[0].evidence, [detail]);
    assert_eq!(
        codes(
            "exited 1",
            "Cannot hand over rendering to Minecraft! The background loading screen renderer seems stuck."
        ),
        ["graphics_handoff"]
    );
}

#[test]
fn neoforge_display_wrapper_does_not_claim_graphics_or_mod_cause() {
    assert_eq!(
        codes(
            "exited 1",
            "Failed to initialize the mod loading system and display.\n\
             Failure details:\nAccess denied trying to create a file or directory"
        ),
        ["unknown"]
    );
}

#[test]
fn early_window_initialization_reuses_the_existing_graphics_family() {
    for detail in [
        "[main/ERROR] [EARLYDISPLAY/]: Failed to find any valid GLFW profile. Trying 4.6: GLFW error: [0x10009]failure",
        "Failed to find a valid GLFW profile.",
        "java.lang.IllegalStateException: Failed to create a GLFW window with any profile",
        "Timed out trying to setup the Game Window.",
        "We seem to be having trouble initializing the window, waited for 30 seconds",
    ] {
        assert_eq!(codes("exited 1", detail), ["graphics"]);
    }
}

#[test]
fn neoforge_dependency_rejections_include_structured_mod_details() {
    let detail = "\tMod ID: 'support', Requested by: 'example', Expected range: '[2,)', Actual version: '[MISSING]'";
    let log = format!(
        "[main/ERROR] [loading/]: Missing or unsupported mandatory dependencies:\n{detail}"
    );
    let findings = analyze("exited 1", Some(&log));
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].code, "mod_dependency");
    assert_eq!(findings[0].evidence.len(), 2);
    assert_eq!(findings[0].evidence[1], detail.replace('\t', ""));
    assert_eq!(
        codes(
            "exited 1",
            "[main/ERROR]: Unsupported installed optional dependencies:"
        ),
        ["mod_dependency"]
    );
    assert_eq!(
        codes("exited 1", "Failure message: Mod §eexample§r requires §6support§r §o2 or above§r\nCurrently, §6support§r is §nnot installed"),
        ["mod_dependency"]
    );
    assert_eq!(
        codes(
            "exited 1",
            "Some mods have requested conflicting versions of: support. Requested by: example."
        ),
        ["mod_dependency"]
    );
}

#[test]
fn fabric_required_dependency_rejection_does_not_include_recommendations() {
    for detail in [
        "- Mod 'Example' (example) 1.0 requires any version of support, which is missing!",
        "- Mod 'Example' (example) 1.0 requires version 2 or later of mod 'Support' (support), but only the wrong version is present: 1!",
        "- Mod 'Example' (example) 1.0 requires any version of support, which is disabled for this environment (client/server only)!",
        "- Mod 'Example' (example) 1.0 requires any version of support, which can't be loaded due to other constraints!",
    ] {
        assert_eq!(codes("exited 1", detail), ["mod_dependency"]);
    }
    assert_eq!(
        codes("exited 1", "[main/WARN]: Mod 'Example' (example) 1.0 recommends any version of support, which is missing!"),
        ["unknown"]
    );
    // Fabric uses this summary for dependency failures as well as actual
    // incompatible combinations; the summary alone stays nonspecific.
    assert_eq!(
        codes("exited 1", "Incompatible mods found!"),
        ["mod_loading"]
    );
}

#[test]
fn explicit_incompatibility_and_wrong_loader_are_distinct_from_missing_mods() {
    for detail in [
        "[main/ERROR]: Incompatibilities between mods:",
        "Mod 'example' is incompatible with 'other', versions: '[1,)'; Version found: '2'",
        "- Mod 'Example' (example) 1.0 is incompatible with any version of mod 'Other' (other), yet a conflicting version is present: 2!",
        "Failure message: File example.jar is a Fabric mod and cannot be loaded",
        "File example.jar is for Minecraft Forge or an older version of NeoForge, and cannot be loaded",
    ] {
        assert_eq!(codes("exited 1", detail), ["mod_incompatible"]);
    }
}

#[test]
fn duplicate_mod_rejection_is_not_a_duplicate_resource_warning() {
    for detail in [
        "[main/ERROR]: Found duplicate mods:",
        "net.minecraftforge.fml.loading.DuplicateModsFoundException: Duplicate mods found",
        "Mod §eexample§r is present in multiple files: example-1.jar, example-2.jar",
    ] {
        assert_eq!(codes("exited 1", detail), ["mod_duplicate"]);
    }
    assert_eq!(
        codes("exited 1", "Duplicate resource pack entry\nFound duplicate textures\nDuplicate mod scan completed without errors"),
        ["unknown"]
    );
}

#[test]
fn fatal_mixin_errors_are_specific_but_optional_mixin_warnings_are_not() {
    for detail in [
        "org.spongepowered.asm.mixin.throwables.MixinApplyError: failed",
        "org.spongepowered.asm.mixin.throwables.MixinInitialisationError: failed",
        "org.spongepowered.asm.mixin.transformer.throwables.MixinTransformerError: critical error",
        "org.spongepowered.asm.mixin.throwables.MixinPrepareError: failed",
        "org.spongepowered.asm.mixin.injection.throwables.InjectionError: Critical injection failure",
    ] {
        assert_eq!(codes("exited 1", detail), ["mixin"]);
    }
    assert_eq!(
        codes("exited 1", "[main/WARN]: Reference map 'example.refmap.json' for example.mixins.json could not be read\n[main/WARN]: Mixin apply failed example: org.spongepowered.asm.mixin.transformer.throwables.InvalidMixinException: optional target unavailable\norg.spongepowered.asm.mixin.transformer.throwables.InvalidMixinException: optional target unavailable\n[main/WARN]: @Mixin target optional.Target was not found"),
        ["unknown"]
    );
}

#[test]
fn nested_causes_precede_wrappers_without_hiding_other_evidence() {
    let log = "net.neoforged.fml.ModLoadingException: loading failed\n\
               We seem to be having trouble handing off the window, tried for 1 second\n\
               Caused by: org.spongepowered.asm.mixin.transformer.throwables.MixinTransformerError: critical error\n\
               Caused by: java.lang.NoClassDefFoundError: example/Support\n\
               Caused by: java.lang.UnsupportedClassVersionError: example.Support";
    assert_eq!(
        codes("exited 1", log),
        [
            "java_version",
            "missing_dependency",
            "mixin",
            "graphics_handoff",
            "mod_loading"
        ]
    );
    assert_eq!(
        codes("exited 1", "net.neoforged.fml.ModLoadingException: loading failed\n[main/ERROR]: Found duplicate mods:"),
        ["mod_duplicate", "mod_loading"]
    );
}

#[test]
fn jvm_native_allocation_failures_keep_the_existing_memory_code() {
    for detail in [
        "# Native memory allocation (malloc) failed to allocate 1048576 bytes. Error detail: AllocateHeap",
        "# Native memory allocation (mmap) failed to map 1048576 bytes. Error detail: committing reserved memory.",
        "Invalid initial heap size: -Xmsnot-a-size",
        "java.lang.OutOfMemoryError: Metaspace",
    ] {
        assert_eq!(codes("exited 1", detail), ["java_memory"]);
    }
}

#[test]
fn ordinary_loader_metadata_and_graphics_progress_do_not_match_new_families() {
    assert_eq!(
        codes("exited 1", "[main/INFO]: Loading 300 mods\n[main/INFO]: Trying GL version 4.6\n[main/INFO]: Requested GL version 4.6 got version 4.6\n[main/DEBUG]: Found 17 mod requirements (4 mandatory, 13 optional)\n[main/DEBUG]: Found 0 mod requirements missing (0 mandatory, 0 optional)\nMod example requires support 2 or above\nCurrently, support is 2.1\nMod example discourages the use of other 1 or above\nMod 'Example' (example) 1.0 conflicts with any version of mod 'Other' (other), which is present with the following version: 2!\nThe window was handed off successfully"),
        ["unknown"]
    );
}
