# Game window title agent

The optional title setting inserts a small Java 8 instrumentation agent before
the Minecraft main class. It replaces only the title argument of the normal
LWJGL 3 `GLFW.glfwCreateWindow` / `glfwSetWindowTitle` `CharSequence` overloads,
and the LWJGL 2 `Display.setTitle(String)` entry point. Every native title call
still happens on the caller's thread. Repeated game title updates receive the
configured title, including on Wayland. This does not change the window's app ID.

The agent does not alter installed game or library files. It has no polling
thread, network access, desktop automation or dependencies outside the Java
standard library. The bridge is visible to isolated game class loaders through
the bootstrap loader. Mods using native pointers, `ByteBuffer`, renamed LWJGL
classes or a different graphics API are outside the supported entry points.
Agent errors are logged without the configured text, then retain the original
class/title. This cosmetic fallback does not claim every game/mod was tested.

The narrowly scoped class-file editor prepends eight bytes to supported methods.
It preserves switch alignment and relocates exception handlers, stack-map frame
deltas, uninitialized-object offsets, and supported debugging attributes. Unknown
code attributes reject the edit so their offsets are never guessed. It does not
require JDK internal ASM exports or open game modules.

`crates/core/assets/game-title-agent.jar` contains only the manifest and Java 8
classes built from `src`. It is embedded in the Rust core, so launcher builds and
game launches do not require `javac`. Regenerate it after changing sources:

```sh
python tools/game-title-agent/build.py
python tools/game-title-agent/build.py --check
python tools/game-title-agent/test.py
```

Regeneration needs JDK 17 or later with `--release 8` support. ZIP timestamps,
ordering, compression and permissions are fixed. The check compares the complete
locally compiled artifact; javac version changes may require reviewing regenerated
bytecode. Tests use synthetic graphics classes, verify Java 8/17 bytecode with
`-Xverify:all`, and can select a runtime with `--java /path/to/java`. They load no
native LWJGL library and open no display or Minecraft installation.

Rust `game_title::prepare_jvm_args` validates the literal text and materializes
only the fixed agent in launcher storage. Existing files must match every byte;
publication never overwrites a name. Unix directory operations use held FDs,
reject symlinks and revalidate directory identities before returning. Empty text
adds no argument and performs no file operation. Cancellation may retain the
fixed, stateless cache; the launch owner's existing cancellation check controls
whether Java is spawned.

References: [Java instrumentation](https://docs.oracle.com/javase/8/docs/api/java/lang/instrument/package-summary.html),
[JVM class-file format](https://docs.oracle.com/javase/specs/jvms/se8/html/jvms-4.html),
[LWJGL GLFW API source](https://github.com/LWJGL/lwjgl3/blob/master/modules/lwjgl/glfw/src/generated/java/org/lwjgl/glfw/GLFW.java).
