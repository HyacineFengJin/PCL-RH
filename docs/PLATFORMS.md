# 平台支持 / Platform support

PCL RH 的完整桌面应用目前只提供 Linux 实现。Windows、macOS 的适配已开始；核心能够解析其 Minecraft 元数据，但这不代表已经可以在这些系统运行完整启动器。

| 部分 | Linux | Windows | macOS |
| --- | --- | --- | --- |
| Minecraft 平台、架构规则与原生库分类 | 已实现 | 已实现 | 已实现，区分 Intel / Apple Silicon |
| Java 路径与官方运行时目录名 | 已实现 | 已实现 `.exe` 路径 | 已实现 JDK / `.bundle` 路径 |
| 有限时 Java 探测 | 已实现，私有进程组 | 已实现，私有 Job Object | 已实现，私有进程组 |
| 账号密钥存储后端 | Secret Service | Windows Credential Store | macOS Keychain |
| 核心、网络与认证模块的原生 CI 测试 | 通过，x64 | 通过，x64 | 通过，Intel / Apple Silicon |
| 声明型插件的清单、授权与持久状态（独立 Node 模块） | 原生 runner 测试通过 | 原生 runner 测试通过 | 原生 runner 测试通过 |
| 原生剪贴板、编辑与默认程序端口 | 已实现，接口 CI 通过 | 已实现，接口 CI 通过 | 已实现，Intel / Apple Silicon 接口 CI 通过 |
| WebUI 生产构建 | 通过 | 通过 | Intel / Apple Silicon 通过 |
| 完整桌面编译与链接 | 通过，x64 | 尚未完成 | 尚未完成 |
| 安装与恢复事务、进程监控、发行包 | Linux 实现 | 尚未适配完成 | 尚未适配完成 |

架构判断不会把 `x86` 当成 `x86_64`，也不会把 ARM 当成 x64。原生库提取使用平台对应的 `.so`、`.dll`、`.dylib` / `.jnilib`；旧元数据中的 `${arch}` 按目标位数展开。Mojang 没有提供的 Java 平台会报告不可用，不会自动下载其他架构的运行时。仅有分类支持不表示所有 Minecraft 版本都有对应架构的原生库。

账号存储使用 [keyring 的平台后端](https://docs.rs/keyring/3.6.3/keyring/#credential-store-features)，账号标识保持兼容。Microsoft 正版服务接入状态仍见[账号说明](AUTHENTICATION.md)。

Java 信息检测只执行选定文件和固定的版本查询参数，按两秒截止时间与每个输出流 64 KiB 上限处理。Windows 在挂起状态建立进程与 Job 的归属后才运行，结束时停止该检测拥有的进程树，另外等待最多 500 毫秒确认清理；无法确认时不会返回可用 Java。Unix 使用私有进程组和直接 `execve`；不会给普通文本隐式补一个 shell，显式声明 `#!` 的包装脚本仍可使用。Windows 批处理不作为 Java 执行。

检测会过滤 `JAVA_TOOL_OPTIONS`、`_JAVA_OPTIONS` 和 `JDK_JAVA_OPTIONS` 的注入，不修改系统环境或已保存的游戏启动参数。

## 核心开发与检查

以下命令只构建和检查独立的核心、网络与认证模块，不构建桌面宿主：

```sh
cargo test --locked -p pcl-core -p pcl-network -p pcl-auth
```

`.github/workflows/portable-core.yml` 在 Linux x64、Windows x64、macOS Intel 与 Apple Silicon 四个原生 runner 上执行该命令。显式平台测试在每个系统检查不同架构的规则与分类；Unix 的进程、权限和符号链接测试仅在 Unix 执行，Windows 另有路径组件检查。测试采用本地夹具与回环服务，不读取真实账号、密钥环或游戏目录。

Java 生命周期检查使用专门的原生可执行夹具，覆盖卡死、输出刷屏、异常退出、保留管道的子进程与其他进程的隔离；Windows 还检查检测方异常退出后的 Job 清理。不需要发现或运行系统 Java，可独立运行：

```sh
cargo test --locked -p pcl-core --test java_probe_lifecycle
```

完整桌面构建的安装、修复和恢复事务仍使用 Linux 文件接口。GTK / WebKitGTK 已限定为 Linux 依赖，官方插件的剪贴板契约不再使用 GTK 窗口类型。其他平台尚无可供日常使用的安装包；当前 `build-native.sh`、`start-native.sh` 与 `install-desktop.sh` 是 Linux 脚本。

## 原生接口

`crates/desktop-platform/` 提供系统剪贴板、网页编辑命令与默认程序打开接口。Windows 使用系统剪贴板和 WebView2，macOS 使用 AppKit，Linux 保留 GTK 异步选区处理。它不管理插件授权、账号或项目；权限、资源识别与调用归属仍由原来的官方插件和命令负责。

可独立检查这些接口，而不构建安装器：

```sh
cargo test --locked -p pcl-desktop-platform
```

GitHub 的 **Native UI ports** 工作流在四个原生 runner 上独立执行这些检查；完整应用仍以 **Desktop platform builds** 的结果为准。

接口测试不访问真实剪贴板、打开浏览器或创建桌面窗口。它们检查文本边界、超时后的单次交付、权限与焦点退役以及迟到回调的归属；编译通过不表示原生交互已经过实机验证。

`pcl-core::file_ops::rename_noreplace` 为安装发布提供三个系统对应的不覆盖重命名原语。它不替代父目录绑定、身份核验、事务日志或中断恢复，不能据此认为整个安装器已经跨平台。

## 桌面构建检查

在 GitHub 的 **Actions → Desktop platform builds → Run workflow** 可手动执行完整桌面编译与链接检查，覆盖 Linux x64、Windows x64，以及 macOS 的 Apple Silicon 和 Intel runner。它先测试原生接口，再构建 WebUI 和启用 `custom-protocol` 的桌面程序；不使用服务密钥、不发布 Release，也不生成供用户安装的发行包。

各平台独立运行。失败会保持红色状态，编译器日志与 Rust 工具链信息保存为该次运行的 artifact，保留 7 天。若依赖安装、接口测试或 WebUI 阶段先失败，应先检查对应步骤日志。当前 Windows/macOS 的安装与文件事务尚未移植，预计会暴露这些阻塞；不会通过跳过桌面模块来将结果改成成功。

三种验证需要分别判断：核心测试通过、完整桌面编译通过、真实系统中的安装与游戏启动通过。前两种检查不能替代最后一种。

构建基线见[核心检查](https://github.com/HyacineFengJin/PCL-RH/actions/runs/40884909298)、[原生接口检查](https://github.com/HyacineFengJin/PCL-RH/actions/runs/40873737517)和[桌面检查](https://github.com/HyacineFengJin/PCL-RH/actions/runs/40872209476)。原生库分类支持的架构范围大于已执行检查的范围，不能把分类支持当成对应架构的实机验证。

## English

The complete PCL RH desktop application currently targets Linux. Cross-platform work has started in the portable core: Minecraft OS/architecture rules, native library classifiers and formats, Java layouts, runtime index keys, and platform-specific credential backends cover Windows and macOS as well.

The portable-core workflow tests the core, network and authentication crates on native Linux x64, Windows x64 and both Intel and Apple Silicon macOS runners. These checks do **not** build the desktop application. Java probing now uses a private Windows Job Object or Unix process group with a two-second deadline and 64 KiB output limit per stream. Windows starts suspended before job assignment and separately bounds cleanup confirmation; Unix uses direct execve to prevent implicit plain-text shell execution. Native executable fixtures exercise process lifetime without an installed JVM. File transactions, process monitoring and distribution packages are not yet adapted for Windows or macOS. Architecture-specific Minecraft natives must also exist for the selected game version. The current build and desktop installation scripts remain Linux-specific.

The manual **Desktop platform builds** workflow separately builds the WebUI and compiles/links the complete desktop application on Linux x64, Windows x64, macOS arm64 and macOS x64. Unsupported native ports fail visibly. Toolchain details and compiler logs are kept as artifacts for seven days; this workflow does not publish releases, use service credentials or verify interactive application/game startup.

The desktop-platform crate isolates native clipboard, edit-command and default-app operations, with GTK dependencies restricted to Linux. Its isolated tests compile/pass on Linux x64, Windows x64 and both macOS architectures without accessing the real clipboard or opening applications. The core also supplies a native no-replace rename primitive; this is not a replacement for directory capabilities or transaction recovery. Full desktop compilation/linking passes on Linux x64; file transactions, installation and process integration still need native ports before Windows/macOS can be distributed.

## 插件模块

社区插件的声明校验、授权、启停、卸载与持久状态是独立 Node 模块，不依赖 GTK、WebKit、AI SDK 或 Linux 文件事务。轻量 stdio 入口也可在没有 AI 运行时的目录中工作；完整 AI 宿主只在需要时加载。

在项目根目录运行：

```sh
node --test experimental/extensions/tests/store-lifecycle.test.mjs experimental/extensions/tests/lightweight-host.test.mjs
```

`.github/workflows/plugins.yml` 为 Linux、Windows、macOS 配置独立插件检查。该检查覆盖插件数据层与协议，不构建桌面窗口、安装器或游戏进程。插件格式见[插件指南](PLUGINS.md)。
