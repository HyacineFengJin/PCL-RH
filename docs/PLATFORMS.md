# 平台支持 / Platform support

PCL RH 已通过 Linux x64、Windows x64、macOS Intel / Apple Silicon 的完整桌面编译与链接。跨平台业务和实机运行仍在验证，当前支持范围与未完成能力如下；标准发行包尚未完成。

| 部分 | Linux | Windows | macOS |
| --- | --- | --- | --- |
| Minecraft 平台、架构规则与原生库分类 | 已实现 | 已实现 | 已实现，区分 Intel / Apple Silicon |
| Java 路径与官方运行时目录名 | 已实现 | 已实现 `.exe` 路径 | 已实现 JDK / `.bundle` 路径 |
| 有限时 Java 探测 | 已实现，私有进程组 | 已实现，私有 Job Object | 已实现，私有进程组 |
| 账号密钥存储后端 | Secret Service | Windows Credential Store | macOS Keychain |
| 核心、网络与认证模块的原生 CI 测试 | 通过，x64 | 通过，x64 | 通过，Intel / Apple Silicon |
| 声明型插件的清单、授权与持久状态（独立 Node 模块） | 原生 runner 测试通过 | 原生 runner 测试通过 | 原生 runner 测试通过 |
| 原生剪贴板、编辑与默认程序端口 | 已实现，接口 CI 通过 | 已实现，接口 CI 通过 | 已实现，Intel / Apple Silicon 接口 CI 通过 |
| 系统内存信息 | 原生读取 | 原生读取 | 原生读取 |
| 保留目录句柄、文件身份与暂存发布原语 | 原生 CI 通过，已接安装与修复 | 原生 CI 通过，已接安装与修复 | 两种架构原生 CI 通过，已接安装与修复 |
| 安装器生产编译与原版 / Fabric 安装、修复事务 | 原生 CI 通过 | 原生 CI 通过 | Intel / Apple Silicon 原生 CI 通过 |
| 独立游戏子进程生命周期 | 已接生产监控器 | 已接独立监控；原生会话、管道截止时间与句柄继承夹具通过 | 原生进程组与独立监控夹具通过 |
| Java 获取、资源另存、实例日志与本机状态存储 | 已实现 | 原生服务检查通过 | Intel / Apple Silicon 原生服务检查通过 |
| 整合包导入、实例导出与导出预设 | Linux 回归通过 | 已接原生端口，完整业务仍待验证 | 已接原生端口，完整业务仍待验证 |
| 模组下载、资源安装 / 更新 / 撤销、实例改名与引用重放 | Linux 业务回归通过 | 原生业务 CI 通过，含内容指纹、取消与旧撤销记录保留 | Intel / Apple Silicon 原生业务 CI 通过 |
| 实例重置、启动准备、内存估算与诊断导出 | Linux 回归通过 | 原生业务 CI 通过 | Intel / Apple Silicon 原生业务 CI 通过 |
| 可恢复实例删除与还原 | Linux 回归通过 | 原生 journal、依赖检查与还原夹具通过 | Intel / Apple Silicon 原生业务 CI 通过 |
| 启动器自身更新 | Linux 回归通过 | 下载、校验与退役夹具通过；运行中 exe 替换未实现 | 原生交换、校验与恢复夹具通过 |
| RH Code 项目扫描与来源绑定 | 回归通过 | 原生文件与来源绑定夹具通过 | Intel / Apple Silicon 原生夹具通过 |
| RH Code 源码应用与资源保存 | 原有事务回归通过 | 尚未实现原生事务，修改前明确拒绝 | 尚未实现原生事务，修改前明确拒绝 |
| RH Code 作者代码构建 | Linux bubblewrap 隔离 | 缺少隔离实现，返回 blocked | 缺少隔离实现，返回 blocked |
| RH Code 可信 Node 宿主与有界 RPC | Linux 回归通过 | 原生锁、管道与退役夹具通过，完整 AI 待实测 | Intel / Apple Silicon 原生夹具通过，完整 AI 待实测 |
| WebUI 生产构建 | 通过 | 通过 | Intel / Apple Silicon 通过 |
| 完整桌面编译与链接 | 原生 CI 通过，x64 | 原生 CI 通过，x64 | Intel / Apple Silicon 原生 CI 通过 |
| 未签名便携测试包 | 归档、解压与 CLI / 插件宿主检查通过 | ZIP 解压与 CLI / 插件宿主检查通过 | Intel / Apple Silicon 归档与解压检查通过 |
| 标准安装包与实机游戏验证 | 待完成发行验证 | 待完成 | Intel / Apple Silicon 待完成 |

架构判断不会把 `x86` 当成 `x86_64`，也不会把 ARM 当成 x64。原生库提取使用平台对应的 `.so`、`.dll`、`.dylib` / `.jnilib`；旧元数据中的 `${arch}` 按目标位数展开。Mojang 没有提供的 Java 平台会报告不可用，不会自动下载其他架构的运行时。仅有分类支持不表示所有 Minecraft 版本都有对应架构的原生库。

账号存储使用 [keyring 的平台后端](https://docs.rs/keyring/3.6.3/keyring/#credential-store-features)，账号标识保持兼容。Microsoft 正版服务接入状态仍见[账号说明](AUTHENTICATION.md)。

Java 信息检测只执行选定文件和固定的版本查询参数，按两秒截止时间与每个输出流 64 KiB 上限处理。Windows 在挂起状态建立进程与 Job 的归属后才运行，结束时停止该检测拥有的进程树，另外等待最多 500 毫秒确认清理；无法确认时不会返回可用 Java。Unix 使用私有进程组和直接 `execve`；不会给普通文本隐式补一个 shell，显式声明 `#!` 的包装脚本仍可使用。Windows 批处理不作为 Java 执行。

检测会过滤 `JAVA_TOOL_OPTIONS`、`_JAVA_OPTIONS` 和 `JDK_JAVA_OPTIONS` 的注入，不修改系统环境或已保存的游戏启动参数。

## 核心开发与检查

RH Code 的独立边界检查见 `.github/workflows/code-boundaries.yml`。它运行真实项目扫描、来源绑定、管道协议及文件夹具；Linux 另运行合成编译与子进程隔离测试。Windows/macOS 的 `blocked` 回执只验证拒绝执行路径，不表示已经支持作者代码构建。该检查也不替代完整 Node 宿主、桌面应用或实机游戏验证。

以下命令只构建和检查独立的核心、网络与认证模块，不构建桌面宿主：

```sh
cargo test --locked -p pcl-core -p pcl-network -p pcl-auth
```

`.github/workflows/portable-core.yml` 在 Linux x64、Windows x64、macOS Intel 与 Apple Silicon 四个原生 runner 上执行该命令。显式平台测试在每个系统检查不同架构的规则与分类；Unix 的进程、权限和符号链接测试仅在 Unix 执行，Windows 另有路径组件检查。测试采用本地夹具与回环服务，不读取真实账号、密钥环或游戏目录。

Java 生命周期检查使用专门的原生可执行夹具，覆盖卡死、输出刷屏、异常退出、保留管道的子进程与其他进程的隔离；Windows 还检查检测方异常退出后的 Job 清理。不需要发现或运行系统 Java，可独立运行：

```sh
cargo test --locked -p pcl-core --test java_probe_lifecycle
```

安装器的文件流、原生库解压、私有工作区、修复登记和恢复已接入原生端口；需要 Java 处理器的 Forge / NeoForge 私有安装仍缺 Windows、macOS 的受限执行后端，会明确拒绝。Java 获取、资源保存、实例改名、重置、删除与还原、引用重放、资源安装与更新的原生业务夹具已通过。启动器自身更新已接 macOS 的原生交换；Windows 能下载、校验与退役更新来源，运行中 EXE 的替换在操作前明确拒绝，尚缺专用 helper。GTK / WebKitGTK 已限定为 Linux 依赖，官方插件的剪贴板契约不再使用 GTK 窗口类型。日常发行包仍在准备；跨平台源码构建可使用 `tools/build-native.py`，Windows 入口为 `start-native.ps1`，Linux/macOS 入口为 `start-native.sh`，`install-desktop.sh` 仅用于 Linux。

## 原生接口

`crates/desktop-platform/` 提供系统剪贴板、网页编辑命令与默认程序打开接口。Windows 使用系统剪贴板和 WebView2，macOS 使用 AppKit，Linux 保留 GTK 异步选区处理。它不管理插件授权、账号或项目；权限、资源识别与调用归属仍由原来的官方插件和命令负责。

可独立检查这些接口，而不构建安装器：

```sh
cargo test --locked -p pcl-desktop-platform
```

GitHub 的 **Native UI ports** 工作流在四个原生 runner 上独立执行这些检查；完整应用仍以 **Desktop platform builds** 的结果为准。

接口测试不访问真实剪贴板、打开浏览器或创建桌面窗口。它们检查文本边界、超时后的单次交付、权限与焦点退役以及迟到回调的归属；编译通过不表示原生交互已经过实机验证。

`pcl-core::file_ops` 提供路径型 `rename_noreplace` 和相对于已持有父目录句柄的 `rename_noreplace_at`。后者只接受单个文件名，不重新解析父目录的可变路径，已接入现有私有安装发布与损坏修复的文件移动。Linux 使用 `renameat2`，macOS 使用 `renameatx_np`，Windows 使用相对目录句柄的原生文件操作；均不通过覆盖、复制再删除或改变工作目录来降级实现。

`pcl-core::retained_fs` 提供持有目录、字面文件名、原生文件身份和私有暂存的共同接口。Windows 使用完整的卷标识与 128 位文件 ID、真实 SID / DACL；Unix 使用设备与 inode。Linux 暂存保持匿名 `O_TMPFILE`，macOS 使用私有命名租约，Windows 保留拒绝其他写入和删除共享的原始文件句柄。命名租约不会被描述为匿名文件。

封存与发布核对大小、SHA256、身份及元数据，并执行实际文件和目录刷新。发布后核验或刷新失败会返回“可能已发布”的结果与回执，禁止直接重试或删除来假装回滚。这些[原语已在四个原生 runner 通过](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41193743734)，包含目录换名、Windows 原地 junction 变更、并发冲突、取消及刷新失败的夹具；这仍不代表整个安装器已跨平台。

下载、缓存复制、组件复制与原生库解压使用保留目录的文件流和暂存输出；网络重试不包含发布错误。修复日志的 Unix 字段与 marker 哈希保持兼容，Windows 日志证据另用真实文件 ID 的显式格式。macOS / Windows 的命名修复暂存单独登记在私有 `dependency-staging`，不会混入旧业务日志。分配记录先持久化才开放写入，发布回执记录失败返回“可能已发布”；日志损坏不能授权删除或覆盖业务原件。已验证的旧 intent 仍可恢复原件并提交原有 marker。

暂存登记有字节、记录与未完成项配额，无法确认时保留现场并停止新写。当前追加会完整复核日志前缀，累计历史较多时的性能仍需优化；这不等于已经具备任意规模、自动回收的长期暂存库。

Windows/macOS 会额外拒绝同一文件的别名改名；如果发布后源名字被其他操作重新创建，也会报冲突，此时文件可能已经发布，调用方须核对事务状态。这个原语不替代父目录准入、源文件身份核验、持久化刷新、事务日志或中断恢复，不能据此认为整个安装器已经跨平台。

**Installer transactions** 工作流在四个原生 runner 先编译完整生产安装器，再运行安装与恢复测试。[四个平台均已通过](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41192854974)。夹具包含实际回环 HTTP、原版 / Fabric 安装、各平台原生 ZIP 提取、取消清理、并发冲突、损坏修复和中断恢复；资源事务包含 512 个真实输入的发布、历史和撤销检查，不访问真实游戏目录。Linux 另有依赖 Landlock 的合成 Java 处理器测试。该结果不是完整桌面编译、真实 Mojang 下载或游戏启动验证。

Windows 的启动器状态文档通过不可变修订日志保存，保留原文件作为身份与内容基线；不会用两次改名模拟原子替换。动态导出预设与导入日志使用调用者显式授予的文档能力，不按任意文件名前缀开放权限。损坏、外部修改或无法确认的提交会保留证据并拒绝覆盖。该机制不能在没有外部锚点时检测整个状态目录同时回滚。

Windows 的状态文档支持显式缺失与重新创建：删除后保存会产生新的逻辑修订，旧状态不能重新授权写入。Unix 的既有日志字段与顺序保持兼容。资源来源的命名暂存由登记凭据持有；已写入退役意图的来源可以恢复清理，只有分配或封存记录的中断来源会保留并阻止新写，不按文件名前缀删除。Windows 的资源终态日志也保留原始基线，仍受历史数量与字节配额约束。

Windows 的游戏目录接口持有本机 NTFS 祖先与非空工作目录租约，已接独立监控器。原生夹具覆盖相对文件操作、目录替换拒绝、GUI 退出后游戏继续运行、精确会话停止与管道退役；它约束拥有者存活期间的目录生命周期，不是任意程序沙箱。macOS 自定义 Seatbelt 规则与 Windows 普通路径工作目录的候选实现出现过实际反例，未作为安全后端启用。

## 桌面构建检查

在 GitHub 的 **Actions → Desktop platform builds → Run workflow** 可手动执行完整桌面编译与链接检查，覆盖 Linux x64、Windows x64，以及 macOS 的 Apple Silicon 和 Intel runner。它先测试原生接口，再构建 WebUI 和启用 `custom-protocol` 的桌面与 CLI 程序，并生成未签名的便携测试包；不使用服务密钥、不发布 Release，也不生成 MSI / `.app` 等标准安装包。

各平台独立运行。失败会保持红色状态，编译器日志与 Rust 工具链信息保存为该次运行的 artifact，保留 7 天。[完整桌面检查](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41192929281)已在四个原生主机上通过编译与链接。具体功能的业务测试、安装包与实机验证仍需分别完成；RH Code 的源码写回和作者代码隔离限制见上表。

三种验证需要分别判断：核心测试通过、完整桌面编译通过、真实系统中的安装与游戏启动通过。前两种检查不能替代最后一种。

[四个平台的便携测试包](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41192929281)已在同一轮构建中通过实际归档、解压、文件哈希、原生可执行架构、CLI 扫描和声明型插件宿主 RPC 检查。[Code 边界回归](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41192854895)也在四个原生主机上通过；Linux 包含实际子进程、namespace init 退出确认、启动前取消和 UTF-8 输出帧顺序检查。这些检查不打开桌面窗口、不启动游戏或模型。下载和依赖要求见[便携测试包说明](BUILDING.md#便携测试包)。

构建基线见[核心检查](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41193743734)、[Code 边界检查](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41192854895)和[桌面检查](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41192929281)。原生库分类支持的架构范围大于已执行检查的范围，不能把分类支持当成对应架构的实机验证。

## English

PCL RH now compiles and links as a complete desktop application on native Linux x64, Windows x64, macOS Intel and Apple Silicon CI hosts. Unsigned portable previews pass archive and headless runtime checks on all four hosts; interactive validation, standard installers and signing remain unfinished. The table above distinguishes verified transactions from unavailable native Code publication and author-code confinement.

The portable-core workflow tests the core, network and authentication crates on native Linux x64, Windows x64 and both Intel and Apple Silicon macOS runners. These checks do **not** build the desktop application. Java probing uses a private Windows Job Object or Unix process group with a two-second deadline and 64 KiB output limit per stream. Windows starts suspended before job assignment and separately bounds cleanup confirmation; Unix uses direct execve to prevent implicit plain-text shell execution. Native executable fixtures exercise process lifetime without an installed JVM. A separate installer workflow compiles the production installer and passes actual vanilla/Fabric installation, repair/recovery, resource publication/undo and instance mutation fixtures on all four hosts. Non-Linux confined Java processors, native Code publication, Windows running-executable replacement, distribution packages and interactive game validation remain unfinished. Architecture-specific Minecraft natives must also exist for the selected game version. The portable source-build entry is `tools/build-native.py`; desktop shortcut installation remains Linux-specific.

The manual **Desktop platform builds** workflow builds the WebUI and compiles/links the desktop and CLI on Linux x64, Windows x64, macOS arm64 and macOS x64. It also assembles unsigned portable previews, verifies their extracted files/native architecture, and runs the actual CLI and declarative host against temporary data. These archive checks have passed on all four hosts. Toolchain details, compiler logs and successful previews are kept as artifacts for seven days; this workflow does not publish releases, use service credentials or verify interactive application/game startup.

The desktop-platform crate isolates native clipboard, edit-command and default-app operations, with GTK dependencies restricted to Linux. Its isolated tests compile/pass on all four hosts without accessing the real clipboard or opening applications. The core supplies retained-directory identity, private staging, bounded sealing, no-replace publication and actual native flushes. Publication uncertainty is explicit. Linux staging is anonymous; Darwin and Windows use named leases with a separate bounded repair registry. Windows records full FileId128 and native SID/DACL evidence. The registry preserves uncertain or corrupt state and does not gain authority over business originals; existing verified recovery intents remain authoritative. Rechecking its full prefix currently has a cumulative performance cost for large histories. Full desktop compilation/linking passes on all four CI hosts; this does not replace interactive application/game verification.

## 插件模块

社区插件的声明校验、授权、启停、卸载与持久状态是独立 Node 模块，不依赖 GTK、WebKit、AI SDK 或 Linux 文件事务。轻量 stdio 入口也可在没有 AI 运行时的目录中工作；完整 AI 宿主只在需要时加载。

在项目根目录运行：

```sh
node --test experimental/extensions/tests/store-lifecycle.test.mjs experimental/extensions/tests/lightweight-host.test.mjs
```

`.github/workflows/plugins.yml` 为 Linux、Windows、macOS 配置独立插件检查。该检查覆盖插件数据层与协议，不构建桌面窗口、安装器或游戏进程。插件格式见[插件指南](PLUGINS.md)。
