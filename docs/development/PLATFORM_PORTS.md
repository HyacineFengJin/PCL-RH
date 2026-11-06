# 跨平台原生接口与检查

面向开发者的实现边界与测试入口。面向使用者的支持范围见[平台支持](../PLATFORMS.md)，安装与更新见[安装与构建](../BUILDING.md)，发行流程见[发行维护](../RELEASING.md)。

以下路径以仓库根目录为基准；工作流链接指向持续更新的检查入口，具体版本的构建与验收范围以对应 Release 为准。

## 平台、Java 与账号

架构判断不会把 `x86` 当成 `x86_64`，也不会把 ARM 当成 x64。原生库提取使用平台对应的 `.so`、`.dll`、`.dylib` / `.jnilib`；旧元数据中的 `${arch}` 按目标位数展开。Mojang 没有提供的 Java 平台会报告不可用，不会自动下载其他架构的运行时。仅有分类支持不表示所有 Minecraft 版本都有对应架构的原生库。

账号存储使用 [keyring 的平台后端](https://docs.rs/keyring/3.6.3/keyring/#credential-store-features)，账号标识保持兼容。Microsoft 正版服务接入状态仍见[账号说明](../AUTHENTICATION.md)。

Java 信息检测只执行选定文件和固定的版本查询参数，按两秒截止时间与每个输出流 64 KiB 上限处理。Windows 在挂起状态建立进程与 Job 的归属后才运行，结束时停止该检测拥有的进程树，另外等待最多 500 毫秒确认清理；无法确认时不会返回可用 Java。Unix 使用私有进程组和直接 `execve`；不会给普通文本隐式补一个 shell，显式声明 `#!` 的包装脚本仍可使用。Windows 批处理不作为 Java 执行。

启动器生成的 Windows Java 路径在当前代码页可无损表示时保留原路径；无法表示时，只接受已经存在且核对为同一文件对象的兼容短名，没有可用短名会明确拒绝，需使用可表示的 Java 或游戏目录，启动器不会修改系统 8.3 设置。启动计划为 Java 提供与受保护原生工作目录对应的可读路径；[真实 JDK 21 回归](https://github.com/HyacineFengJin/PCL-RH/actions/workflows/portable-core.yml)已验证中文目录的相对读写和原有目录保护。

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

安装器的文件流、原生库解压、私有工作区、修复登记和恢复已接入原生端口。通过私有暂存重建游戏核心的整合包安装，在 Windows/macOS 上仍缺 Java 处理器的受限执行后端；检查计划时会依据实际客户端处理器记录给出阻止原因，在下载 Java 或创建私有实例前拒绝。原版、Fabric、没有客户端处理器的计划及无需重建核心的完整游戏包不受这项限制。Linux 仍在工作线程中验证实际 Landlock/openat2 能力。

「下载游戏」的普通组件安装沿用用户选择目标目录与加载器的原有授权和 Java 处理器流程；它与上述私有安装、RH Code 作者代码隔离是不同入口。普通路径没有因这项平台检查而被禁止，也不能据此认为任意 Forge/NeoForge 版本已通过三平台实机验证。

Java 获取、资源保存、实例改名、重置、删除与还原、引用重放、资源安装与更新的原生业务夹具已通过。便携版自身更新已接 macOS 的原生交换；Windows 能下载、校验与退役更新来源，运行中 EXE 的替换在操作前明确拒绝，尚缺专用 helper。安装版由原分发方式更新：Linux 使用包管理器，macOS 替换完整 `.app`。GTK / WebKitGTK 已限定为 Linux 依赖，官方插件的剪贴板契约不再使用 GTK 窗口类型。跨平台源码构建可使用 `tools/build-native.py`，Windows 入口为 `start-native.ps1`，Linux/macOS 入口为 `start-native.sh`，`install-desktop.sh` 仅用于 Linux。

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

封存与发布核对大小、SHA256、身份及元数据，并执行实际文件和目录刷新。发布后核验或刷新失败会返回“可能已发布”的结果与回执，禁止直接重试或删除来假装回滚。这些[原语已在四个原生 runner 通过](https://github.com/HyacineFengJin/PCL-RH/actions/workflows/portable-core.yml)，包含目录换名、Windows 原地 junction 变更、并发冲突、取消及刷新失败的夹具；这些原语检查不能替代完整安装流程和实机验收。

下载、缓存复制、组件复制与原生库解压使用保留目录的文件流和暂存输出；网络重试不包含发布错误。修复日志的 Unix 字段与 marker 哈希保持兼容，Windows 日志证据另用真实文件 ID 的显式格式。macOS / Windows 的命名修复暂存单独登记在私有 `dependency-staging`，不会混入旧业务日志。分配记录先持久化才开放写入，发布回执记录失败返回“可能已发布”；日志损坏不能授权删除或覆盖业务原件。已验证的旧 intent 仍可恢复原件并提交原有 marker。

暂存登记有字节、记录与未完成项配额，无法确认时保留现场并停止新写。当前追加会完整复核日志前缀，累计历史较多时的性能仍需优化；这不等于已经具备任意规模、自动回收的长期暂存库。

Windows/macOS 会额外拒绝同一文件的别名改名；如果发布后源名字被其他操作重新创建，也会报冲突，此时文件可能已经发布，调用方须核对事务状态。这个原语不替代父目录准入、源文件身份核验、持久化刷新、事务日志或中断恢复，完整安装流程仍须独立验证。

**Installer transactions** 工作流在四个原生 runner 先编译完整生产安装器，再运行安装与恢复测试。[四个平台均已通过](https://github.com/HyacineFengJin/PCL-RH/actions/workflows/installer-transactions.yml)。夹具包含实际回环 HTTP、原版 / Fabric 安装、各平台原生 ZIP 提取、取消清理、并发冲突、损坏修复和中断恢复；资源事务包含 512 个真实输入的发布、历史和撤销检查，不访问真实游戏目录。Linux 另有依赖 Landlock 的合成 Java 处理器测试。该结果不是完整桌面编译、真实 Mojang 下载或游戏启动验证。

Windows 的启动器状态文档通过不可变修订日志保存，保留原文件作为身份与内容基线；不会用两次改名模拟原子替换。动态导出预设与导入日志使用调用者显式授予的文档能力，不按任意文件名前缀开放权限。损坏、外部修改或无法确认的提交会保留证据并拒绝覆盖。该机制不能在没有外部锚点时检测整个状态目录同时回滚。

Windows 的状态文档支持显式缺失与重新创建：删除后保存会产生新的逻辑修订，旧状态不能重新授权写入。Unix 的既有日志字段与顺序保持兼容。资源来源的命名暂存由登记凭据持有；已写入退役意图的来源可以恢复清理，只有分配或封存记录的中断来源会保留并阻止新写，不按文件名前缀删除。Windows 的资源终态日志也保留原始基线，仍受历史数量与字节配额约束。

Windows 的游戏目录接口持有本机 NTFS 祖先与非空工作目录租约，已接独立监控器。原生夹具覆盖相对文件操作、目录替换拒绝、GUI 退出后游戏继续运行、精确会话停止与管道退役；它约束拥有者存活期间的目录生命周期，不是任意程序沙箱。macOS 自定义 Seatbelt 规则与 Windows 普通路径工作目录的候选实现出现过实际反例，未作为安全后端启用。

## 插件模块

社区插件的声明校验、授权、启停、卸载与持久状态是独立 Node 模块，不依赖 GTK、WebKit、AI SDK 或 Linux 文件事务。轻量 stdio 入口也可在没有 AI 运行时的目录中工作；完整 AI 宿主只在需要时加载。

在项目根目录运行：

```sh
node --test experimental/extensions/tests/store-lifecycle.test.mjs experimental/extensions/tests/lightweight-host.test.mjs
```

`.github/workflows/plugins.yml` 为 Linux、Windows、macOS 配置独立插件检查。该检查覆盖插件数据层与协议，不构建桌面窗口、安装器或游戏进程。插件格式见[插件指南](../PLUGINS.md)。
