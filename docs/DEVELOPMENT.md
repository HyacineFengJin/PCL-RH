# 代码结构与排查入口

这份说明面向阅读、修改和排查 PCL RH 代码的开发者，描述当前实现。复杂事务的状态与约束放在对应 Rust 模块的开头，修改实现时应同步更新。

## 从哪里开始读

| 职责 | 入口 | 主要内容 |
| --- | --- | --- |
| 前端状态与桌面调用 | `apps/desktop/src/main.tsx` | 页面、当前目录、bootstrap、作用域 API 与响应接纳 |
| RH Code 模块与项目 | `rh-code/`、`rh_code.rs`、`code/` | Cordis 内置插件、可拆分区域、原生项目服务、对话任务绑定；详见 [RH Code 模块结构](RH_CODE_ARCHITECTURE.md) |
| 实例管理界面 | `InstancesPanel.tsx`、`InstanceOperations.tsx`、`InstanceImport.tsx`、`InstanceTrash.tsx` | 实例资料、改名、修改、导出、ZIP 导入与删除恢复 |
| 下载与任务界面 | `useDownloadTasks.ts`、`DownloadPanel.tsx`、`TaskManager.tsx` | 集合轮询、逐项取消、完成刷新与页面所有权 |
| 在线资源界面 | `ResourceDetails.tsx`、`ResourceInstall.tsx`、`resourceInstallPlan.ts`、`LocalResources.tsx`、`ResourceUpdates.tsx` | 版本与文件选择、作用域确认、必要依赖计划和安装提交 |
| Java 管理界面 | `JavaPanel.tsx`、`JavaSelect.tsx`、`javaManagement.ts` | 全局与实例选择、目录/修订作用域、过期响应排除 |
| 启动诊断与补全界面 | `LaunchInspection.tsx`、`LaunchFailure.tsx`、`LauncherLogs.tsx`、`TaskManager.tsx` | 环境检查、失败线索与精确日志入口、缺失文件任务与部分补全清单 |
| 桌面命令与应用生命周期 | `apps/desktop/src-tauri/src/main.rs` | 命令参数、目标绑定、任务调度、游戏进程与关闭处理 |
| 本地实例操作命令 | `instance_commands.rs` | ZIP 导入与删除恢复的目录绑定、计划确认、任务准入与错误归类 |
| 在线资源命令 | `resource_install_commands.rs`、`resource_update_commands.rs` | 目标绑定、后台任务、传输进度、提交检查与根目录恢复 |
| 改名编排 | `instance_rename_service.rs` | 组合文件/资料 revision、进度、错误归类与缓存刷新 |
| 持久设置与实例资料 | `config.rs`、`instance_meta.rs`、`export_presets.rs` | 多目录、选择与内存、元资料、导出配置 |
| Java 桌面服务 | `java_commands.rs`、`java_service.rs`、`platform.rs` | 选择器、登记请求绑定与探测前后校验 |
| 文件操作 | `resource_ops.rs`、`instance_reset.rs`、`instance_export.rs`、`instance_import.rs`、`instance_delete.rs` | 本地资源事务、组件重置、ZIP 导出/导入、实例删除恢复 |
| 任务协调 | `tasks.rs`、`tasks/schedule.rs`、`tasks/scope.rs`、`downloads.rs` | 并发/队列、物理路径互斥、取消与结束、带修订号的集合投影 |
| Minecraft 核心 | `crates/core/src/lib.rs` | 版本识别、继承、依赖和启动参数 |
| 启动依赖与诊断 | `crates/core/src/dependencies.rs`、`inspection.rs` | 启动共用的依赖筛选、文件存在性和 Java 检查 |
| 缺失文件补全 | `launcher_repair.rs`、`crates/core/src/repair.rs`、`crates/install/src/repair.rs` | 原生任务绑定、纯下载清单、固定目录下载与发布 |
| 平台与元数据规则 | `crates/core/src/platform.rs`、`rules.rs` | 系统／架构词汇、原生库与 Java 布局、安装和启动共用的规则 |
| Java 核心 | `crates/core/src/java.rs` | 有限时探测、发现、架构与版本策略、启动选择 |
| 游戏启动选项 | `GameLaunchFields.tsx`、`GameIdentityFields.tsx`、`GameArgumentFields.tsx`、`crates/core/src/launch_options.rs` | 窗口尺寸、Java 地址偏好、标题、实例覆盖、附加参数解析与版本信息 |
| 游戏内容目录 | `crates/core/src/content_scope.rs` | 自动隔离标记、内容作用域、各文件服务共用的判断 |
| 下载与安装 | `crates/install/src/`、`resolved.rs` | 网络请求、校验缓存、原版与加载器安装、固定提供者证据 |
| 本地整合包 | `instance_import/mrpack/`、`instance_import/build.rs`、`publication.rs` | 格式适配、原生一次性确认、私有构建和可恢复提交 |
| Modrinth 安装 | `modrinth_install/` | 官方元数据、兼容与必需依赖规划、实例快照、匿名网络暂存 |
| 资源批次提交 | `resource_ops/verified_batch/`、`resource_ops/update_batch/` | 新资源提交、更新备份与撤销、所有权登记、回滚和中断恢复 |
| 账号 | `crates/auth/src/lib.rs`、桌面的 `accounts.rs` | 认证协议、密钥环、账号状态与刷新 |
| 资源收藏 | `launcher_favorites.rs`、`launcher_favorite_commands.rs`、`useLauncherFavorites.tsx` | 稳定提供者身份、文件夹、视图修订号与批量原子保存 |
| 百宝箱文件与图片 | `toolbox_download/`、`toolbox_images/`、`local_resource_info/` | 用户选择的目标、下载取消、图片像素与只读元数据快照 |

表中未写目录的 Rust 文件位于 `apps/desktop/src-tauri/src/`；TSX 文件位于 `apps/desktop/src/`。现有桌面和前端 `main` 仍承担较多协调工作；新增业务应先确定职责归属，避免继续堆入页面或命令函数。

跨平台范围见[平台支持](PLATFORMS.md)。`core::platform` 只描述平台与路径布局，不拥有文件事务权限；桌面的 `platform.rs` 仍负责系统选择器，二者职责不同。`rules::RuleContext` 同时服务安装依赖筛选和启动参数；现代 native classifier 进一步约束系统与架构，旧版 native 表按 Mojang 的 `linux` / `windows` / `osx` 键选择。生产调用始终取当前编译目标，测试可提供显式平台。不要用另一个平台的规则结果驱动本机文件事务。

## 缺失文件补全

`LaunchInspection` 的报告只供展示；补全命令只接收登记的根目录、实例 ID 和设置修订号。`launcher_repair` 获取独占文件任务后，在操作锁外重新捕获版本继承链、资源索引和目录身份。`core::repair` 负责路径、当前平台规则和下载声明，不执行文件操作；`install::repair` 才持有目录描述符和下载权限。

下载复用安装器的官方 HTTPS 来源检查、网络设置、共享限速和取消机制。每个缺失文件先写匿名临时文件，再验证大小与 SHA1，并在发布前复核绑定。原子发布拒绝覆盖已有目标；取消丢弃未发布的文件，已发布文件可保留复用。开始和结束重读元数据内容，中间检查目录身份和包含 ctime 的文件状态，避免每个资源文件都重读整个索引。

`TaskOutcome::Incomplete` 保留结构化结果并进入 Error，让无法自动处理的文件留在任务卡上。普通取消、部分补全和完整补全都可能留下新文件，因此终态只刷新对应根目录的视图；完成不修改实例选择。诊断弹窗关闭后，迟到的任务提交回复仍交给全局任务管理，但不能重新打开已经离开的页面。

排查入口为 `core/src/repair_tests.rs`、`install/src/repair_tests.rs`、`launcher_repair/tests.rs` 及 `launch-repair-*.test.mjs`。补全只处理缺失文件，不运行 Java 或加载器处理器，不验证既有文件的内容，也不取得修改模组或存档的权限。

## 以实例改名为例

阅读顺序：

1. `InstancesPanel.tsx` 的检查与确认：客户端提交名称和确认 revision。
2. 桌面 `main.rs` 的 `instance_rename_prepare`、`instance_rename_start`：解析已登记目录、检查游戏/任务占用，重新准备并核对 revision，再捕获目标并启动工作线程。
3. `instance_rename_service::prepare` 和 `CheckedRename`：把物理文件计划、启动器引用和组合 revision 明确放在一起。引用载荷保留在 Rust 内部。
4. `instance_rename::execute`：验证快照、暂存 JSON、持久登记、移动核心与目录、提交文件状态。完整恢复状态表在该模块开头。
5. `instance_rename_refs::apply`：按固定目标更新设置、元资料、导出配置和撤销记录。每个目标接受预期的修改前或修改后状态，以便重启后继续完成部分迁移。
6. 服务刷新内存中的设置/元资料，命令记录最终任务结果，前端重新读取 bootstrap。

`instance_rename.rs` 管理游戏文件与事务阶段；`instance_rename_refs.rs` 管理启动器资料和项目级待恢复标记；`instance_rename_service.rs` 把两者接到应用任务与缓存。前端导航可以变化，已提交任务始终使用捕获的目标目录。

### 排查改名与恢复问题

- **检查或确认被拒绝**：先看服务的组合 revision，再分别检查文件计划与引用快照；不要通过移除校验来绕过目标变化。
- **取消被接受但结果异常**：查看任务的 `begin_finishing` 和事务的 `committing` 回调。关闭取消接纳后必须再读一次已接受的 token，第一处文件移动后按事务失败/恢复处理。
- **目录已是新名，但恢复要回旧名**：以持久 journal 状态为准。目录移动与 `FilesCommitted` 的持久登记之间存在崩溃窗口。
- **文件改名完成、资料只改了一部分**：查看 `FilesCommitted` 的引用重放。此时应继续完成引用；回滚文件会让已经迁移的引用指向不存在的名字。
- **资料保存或恢复报外部修改**：检查目录身份、文件快照和待恢复标记。保留冲突文件与备份；不直接删除标记或放宽哈希/身份比较。
- **界面重新出现旧选择或内存设置**：检查 config 的 transport revision、`save_settings` 返回值及前端响应接纳条件。保存成功后的新 revision 必须被采用；旧响应不能覆盖已经更新的 bootstrap。
- **切到另一个目录后恢复按钮不可用**：检查 bootstrap 中待恢复的根目录匹配。登记路径可以是别名，事务绑定的是规范路径和目录身份。

## Java 的数据与启动路径

`config.rs` 的 v3 保存全局 `java`、登记路径 `java_paths` 与各目录的 `java_overrides`。缺少实例覆盖表示跟随全局；显式 `Auto` 表示该实例独立自动选择。`Settings` 只投影当前目录的覆盖，启动使用捕获的 `GameRoot`，避免切换目录后读取另一个同名实例的选择。普通设置保存不能直接替换 Java 路径登记表。

添加流程从 `JavaPanel` 经 `java_commands::java_add`、`platform::pick_java` 到 `java_service::register`。目录 ID 和 transport revision 在弹窗前捕获，在执行探测前和提交登记前各检查一次。探测运行在阻塞工作线程中，期间不持有操作互斥锁。添加成功的设置由前端应用入口接纳；原页面卸载后仍需更新已提交的登记表和 revision。

官方下载入口是 `JavaDownloads` 和 `java_download_commands.rs`。提交只携带组件与 manifest SHA-1，不接受渲染端 URL。`java_download/catalog.rs` 解析 Mojang 平台目录并验证完整文件计划；`network.rs` 捕获网络和下载调度快照，限制元数据、文件大小、等待时间与来源；`files.rs` 使用私有目录描述符、匿名文件、持久所有权记录和不覆盖的整目录发布。内部链接只能引用清单内项目，不能成为写入或清理的父目录。

下载在全局任务队列内占用托管 Java 目录。探测主版本与架构之后关闭取消接纳，再读已接纳的取消 token，随后发布目录。校验或清理错误优先于取消；被修改的暂存保留。下次下载持有过程锁后清理可识别的中断暂存。完整目录一旦发布就不能因后续登记或同步失败而删除；核心发现也扫描完整目录，覆盖发布到登记之间的崩溃窗口。

登记通过 `ConfigStore::register_downloaded_java` 合并到最新配置，只追加路径，不重放提交时的目录、revision 或 Java 选择。任务完成或保留安装的错误会刷新当前 bootstrap；晚到的提交响应仍登记任务，但不能重新打开旧页面。故障回归集中在 `java_download/tests.rs`，使用 loopback 官方响应和脚本运行时，不需要下载或执行真实 JVM。

`crates/core/src/java.rs` 统一列表与启动选择。每次探测限制时长和输出，自动发现限制候选数量及总时长。程序使用独立进程组，超时或退出后清理组内子进程，并等待直接子进程；不要改成持锁执行或无界管道读取。手动选择在启动时重新探测，错误不会退回自动选择。Forge/NeoForge 的主版本策略也在核心执行，前端禁用不兼容选项只是提前提示。

启动前的自动获取由 `launcher_preparation/java.rs` 管理。核心 `inspection::java_requirement` 与 `java::select_from_catalog` 提供类型化需求和本地选择结果，只在 Auto 且无兼容候选时创建全局 `JavaInstall` 任务。官方下载复用 guarded engine；最终以捕获的设置 revision 调用 `ConfigStore::register_java`，只接受这次追加返回的新 revision，再把更新的 Settings 传给同一启动代次的构建闭包。手动选择不回退，账号身份仍只在最终构建阶段读取；取消、下载、登记或重验失败均不提交游戏进程。

加载器处理器的 Java 由 `crates/install/src/components.rs::installer_java` 选择，与上述游戏启动选择分开。`Installer::with_java(path)` 是严格指定：路径失效、探测失败或主版本不符时直接报错，不继续寻找其他 Java；没有调用该方法时保留自动发现。不要把显式路径作为自动候选的优先提示，否则调用者确认的运行环境会被静默替换。桌面普通安装、整合包和重置通过 `installer_java::Context` 复用捕获的实例／全局选择与网络策略。`ResolvedInstallRequest::processor_java_requirement` 与执行器共用客户端处理器判定，无处理器不探测或下载；自动模式缺少精确主版本才调用官方 guarded 下载引擎，手动选择通过 `with_java` 保持严格语义。

Java 获取沿用原安装任务与取消 token，在实例私有构建或重置 journal 之前完成。普通安装与整合包在排队前通过 `TaskScope::union` 同时保留目标根和托管 Java 子树；缺失子树只读捕获现存祖先，不预建目录。重置继续使用原全局 writer。Java 策略检查只绑定目标登记路径、有效选择和候选路径；前序安装自动选择实例或无关偏好更新不会使排队任务失效。网络工厂与调度器仍使用提交快照。Java 的完整目录发布不会关闭父安装任务的取消接纳，失败后保留完整 runtime，由核心自动发现，无须改写设置 revision。后续安装始终使用同一份原生 resolved metadata，Java 网络字节累加到原任务；外层事务进入提交后仍保留自己的提交语义。

v2 设置在普通启动时备份并迁移。已有改名 journal 的引用载荷必须按原 v2 字段顺序重放；存在项目待恢复标记时，只在内存中规范化，保留磁盘字节。完成恢复后的首次合法设置写入再备份恢复后的原始 v2 字节并升级。排查升级与改名交叉问题时，先读 `VersionTwo`、`rename_bytes` 和 `pending_migration`，不要让新字段提前改变 journal 的预期快照。

## 游戏窗口与地址偏好

全局选项保存在 `LauncherPreferences::game_launch`，沿用启动器偏好修订号和部分更新；旧文件与旧设置备份缺少该字段时使用默认值。窗口字段的反序列化使用空结构体变体，避免 Serde 的带标签空枚举变体忽略未知字段。无效或未来格式保留原文件，准备游戏时返回偏好存储的警告。

`main.rs::captured_launch_preferences` 在接纳启动准备前捕获已验证的完整偏好，与游戏目录、内存和 Java 选择一起交给工作线程。检查实例也使用相同选项。`build_launch_plan_*_with_options` 接收这份快照，旧核心 API 继续使用默认值；命令行不会读取桌面偏好文件。

窗口尺寸只影响游戏参数规则中的 `has_custom_resolution`，不改变库选择或内容目录。核心展开新版条件参数或旧版参数后统一替换 `--width` / `--height`，防止重复尺寸；默认模式保留原参数。地址偏好在主类之前替换冲突的 JVM 属性，使用 `preferIPv6Addresses` 决定地址顺序，并保持 `preferIPv4Stack=false` 以允许双协议连接。属性含义见 [Java 网络属性说明](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/net/doc-files/net-properties.html)。

前端自定义尺寸是未提交的本地草稿；只有明确保存才更新窗口字段。IP 或其他偏好修订变化不能重置尺寸草稿，旧修订或卸载页面的事件不能重新提交。对应回归覆盖偏好读写、备份导入、启动快照，以及新旧版本参数和账号令牌脱敏。

`custom_info`、`jvm_args`、`game_args` 的全局值同样位于 `game_launch`；每个 `GameRoot::argument_overrides` 使用可选字段区分继承与显式清空。配置加载、保存和原生启动捕获均验证同一核心类型，改名迁移其所属实例 key，不猜测自由文本中的路径。`captured_game_options` 合并覆盖后整体校验，失败不部分采用或退回默认。预检验证配置，不运行 JVM 验证选项支持情况。

`launch_options` 的有界分词器不调用 shell；JVM 参数保护主类、classpath 和堆大小所有权，游戏参数保护账号、版本及内容路径。附加 JVM 参数在主类前，游戏参数在窗口归一化前追加。版本参数只展开原始模板一次，替换值中的 `${...}` 保持字面量；分词及未知占位符错误不回显输入。前端每项显式保存，沿用修订与页面身份退役旧回调；无关字段的修订变化不会抹掉未保存草稿。

## 内容目录的所有权

`content_scope::resolve_scope` 根据捕获的 `IsolationPolicy` 与四个隔离标记决定共享/实例目录；`automatic_scope` 保留默认自动模式包装。每个调用者提供自己的文件探测。所有标记都要读取，已有普通 `mods` 不能遮住后面的坏链接或读取错误。只有不存在表示缺少标记，符号链接和特殊文件不能被解释为共享目录。

全局策略与按游戏目录、实例名登记的覆盖保存在 `config.rs`，缺省为自动识别。`IsolationSettings` 一次捕获设置修订、原登记路径与规范化根目录；原生命令及其确认、队列和最终发布都检查捕获代次。隔离变化须等待游戏和全部文件任务结束并通过恢复检查。核心扫描与启动通过只读路径适配器判定；启动在 Java 探测之前捕获内容目录，并在准备结束时重检隔离状态。`scan_instance` 单独读取所选实例、保留具体错误，资源页和社区目标检查不再从过滤后的实例列表推断“未找到”。其他实例的损坏仍由批量扫描报告独立列出。

ZIP 导出的 `Scanner` 与资源信息的 `Binding` 继续使用各自的描述符探测、缺失记录和身份快照，把分类交给共用规则；不能用核心的路径读替换这些证据。`ContentScope::relative_base` 只提供经过名称校验的布局，不授予文件操作权限。已有资源、更新、导入和改名恢复记录保持原格式和提交时的目录绑定；恢复时不能因当前隔离状态改变而重定向旧事务。

游戏标题使用 `game_title::prepare_jvm_args` 在主类之前添加内嵌的 Java 8 agent；默认标题不访问缓存或添加参数。实例标题覆盖与隔离覆盖一起绑定设置修订，空标题表示游戏默认、缺少覆盖表示继承全局。固定 agent 的源码、可复现构建与合成 API 检查见 [`tools/game-title-agent`](../tools/game-title-agent/README.md)。缓存只含固定字节，未知文件及不安全路径会阻止生成自定义标题启动计划。

## ZIP 导入与实例删除

`instance_commands.rs` 只负责桌面准入和任务生命周期。导入前重新创建文件计划，比较 revision 后再启动工作线程；选择器路径和序列化的计划摘要不能作为已验证文件的替代品。事务模块检查来源身份、内容与目标快照，拒绝覆盖，拥有提交和清理记录。取消请求只有在事务明确返回取消时才归为 Cancelled；其他错误使用 `TaskOutcome::Error`，避免迟来的取消掩盖冲突或校验失败。

导入将继承描述合并成独立实例，不把共享内容写回新目录之外的任意路径。公共库和素材只允许相同内容复用。排查时从 `instance_import::prepare`、`execute_checked`、`recover_pending` 开始，核对提交前的来源重检、启动器旧引用检查、暂存所有权登记和中断恢复。`archive.rs` 处理归档结构、格式和版本继承；`filesystem.rs` 处理目录描述符、文件身份与快照；主模块负责计划、持久状态和提交编排。

暂存文件先以匿名文件写入并校验，再登记所有权，最后给它发布文件名。每个文件的登记是独立小记录，完整计划只在状态切换时保存，避免文件较多的整合包反复序列化整份计划。恢复会合并这些登记，按文件身份和哈希核对后才清理；校验或清理失败不能被取消请求遮盖。

删除事务移动 `versions/<id>`，不修改启动器引用。删除 journal 保留名称 reservation，直到原文件恢复成功；安装、导入、改名、启动和资源操作必须检查 reservation。bootstrap 排除外部重新创建的同名目录，防止旧元资料附到不同实例。设置、资料和资源历史的旧引用也不能被新实例默默继承；包括已清空资料的 generation 记录，它仍用于排除旧确认 token。

删除和恢复在 settings flock 后取得恢复区锁，提交前重检 Java 依赖和实例引用。项目待恢复 marker 跨游戏目录阻止资料写入；恢复必须绑定原项目及规范目录。恢复记录仅允许已登记状态的重放或明确的空暂存清理，遇到外部修改保留文件。清理日志临时文件前也要验证恢复目录身份，不能先清理再拒绝外部替换的目录。

`instance_delete/filesystem.rs` 负责目录描述符、内容树快照、锁、移动与空目录清理；主模块负责依赖检查、计划、日志状态和删除/恢复。文件事务回归分别在 `instance_import/tests.rs` 和 `instance_delete/tests.rs`；bootstrap、旧资料及外部重建的组合行为在 `main.rs` 的应用集成用例中。桌面命令在大型归档/内容树的只读校验期间释放 `operations`，返回方案或准入任务前再检查占用、目录绑定和待恢复状态。

## 损坏依赖修复与恢复

`IntegrityCheck` 保持只读，`MissingRepair` 保持只补缺失。显式写入由 `DamageRepair` 重新捕获声明、校验已有内容；`damage_repair.rs` 编排下载与有界报告，`damage_store.rs` 持有目录 FD、独立原件副本、不可变 intent 与 commit/rollback 标记。每文件替换都先同步备份和 intent，再用无覆盖移动发布；未提交部分可恢复，提交完成部分保留。原件指纹使用 SHA256，下载验证沿用声明的 SHA1 与大小；不接受任意模组或存档路径。

桌面 `launcher_damage` 复用已绑定的根目录、设置和任务 guard，普通修复只排除自身 operation ID；恢复是根目录任务，不要求原实例仍存在。未完成记录阻止该目录的其他写入和启动，bootstrap 给出恢复入口。结果保留在任务卡上，由用户收起。`DamageRepair::ensure_ready` 对不存在的存储先作无创建探测，健康启动不会因此新增修复能力要求。事务状态回归见 `damage_repair_tests.rs`，准入与恢复入口见 `launcher_damage_tests.rs`。

## 社区来源与项目身份

`community_catalog` 编排来源搜索、部分失败和百科取证；`community_identity` 只用提供方 ID 与明确项目链接建立关系。名称、作者和跨站相同 slug 不构成匹配证据。关系组件含多个同站项目时拒绝跨站归并，避免百科宽泛词条合并分支。版本筛选下百科仅作为证据，不能增加未过滤的结果。

`community_mcmod` 只解析公开搜索／列表、标题和相关链接区域，不执行页面脚本。详情取证有请求、并发、时间和缓存预算；HTML 改版或响应异常时保留其他来源。`community_curseforge` 使用固定官方 API、敏感请求头、有限分页和独立错误。`community_details` 通过精确官方 slug 查询把百科链接解析为真实 ID；有歧义时保留资料入口。前端 `communityResources` 分开处理项目身份、来源别名与图标域名，来源切换退役旧详情与保存操作。

社区检索的 `page` 缺省为 1。Modrinth 使用受检查的 offset 与真实 total_hits，CurseForge 使用已验证的 index/resultCount/totalCount，百科沿用 HTML 分页证据；`pagination.has_next` 不从归并后的卡片数推算。CurseForge 的 index + pageSize 不超过官方 10,000 条边界，百科保持 100 页范围，这些限制只停止对应来源并给出提示。证据用途的百科列表不能延长已结束的筛选结果。`communityPagination.ts` 按查询保留最多 8 页原始结果，仅用真实来源 ID 与已确认链接对较早的已加载页去重，同站真实 ID 冲突保留独立。返回旧页不会被全局 seen 集合过滤；被淘汰的页重新请求。查询与页面分别绑定对象身份，根目录、API、关键词和筛选变化即时退役旧回复及事件处理函数。

`resource_save` 按来源重新读取精确文件权限与元数据。CurseForge 的 SHA-1 与 Modrinth 的 SHA-512 由 `resource_digest` 明确区分，不将一种校验值冒充另一种。下载和落盘复核都校验官方摘要；本地 SHA-512 另行计算用于保存回执。保存继续使用已捕获目录、匿名暂存、最终元数据重验和无覆盖发布。CurseForge 社区资源尚不进入 Modrinth 的依赖安装／更新事务；本地 CurseForge ZIP 的精确文件引用由独立的 `mrpack/curseforge.rs` 管线解析，仍要求已配置的官方 API 服务。

## 整合包与 Modrinth 资源安装

整合包入口由 `instance_commands.rs` 与 `InstanceImport.tsx` 共享。PCL 导出 ZIP 保留原 DTO 和事务；其他格式由 `instance_import/mrpack.rs` 与 `formats.rs` 归一为客户端有效输出。`archive.rs` 限制 ZIP 索引、路径和解压容量，所有归档条目都校验；适配器只选择游戏内容，不导入启动器资料。完整目录包合并原核心继承，其余格式根据清单重建受支持组件。本地 CurseForge ZIP 的远程文件编号按精确项目与文件 ID 请求官方元数据；服务未配置、禁止分发或缺少官方地址时明确阻止，不能静默遗漏。

带启动器归档始终保留用户所选外层文件作为来源权威。`prepare_checked_with_stage` 从严格中央目录定位唯一内包，调用方先预留磁盘与 FD，再将内包流式写入匿名文件；外层所有条目仍校验 CRC，内层沿用自己的格式与大小限制，两层共享解码预算。`CheckedPack::content_file` 是覆盖文件和完整游戏核心的唯一内容入口，源重查同时检查外层路径/FD 和内层 FD。计划摘要包括内层身份、哈希及外层条目事实，不以 `!/` 拼路径或序列化临时文件名。

`service.rs::Preparation` 在分类前取得有界准备窗口和暂存额度；匿名输入在项目目录的独立 staging 中创建，实际位置与游戏目录重叠时拒绝准备。额度贯穿未提交确认、已领取队列与工作线程，取消、错误和丢弃关闭输入后归还。`PendingProjection` 在原生命令的最终目录/名称/关闭门禁失败时，只移除本次尚未领取的确认；成功投影或已经领取的任务不受撤销影响。恢复仍处理输出事务，匿名输入不成为可跨重启继续的授权。

`mrpack/official.rs` 处理社区详情页的官方 `.mrpack` 输入。`resource_save/authority.rs::resolve_pack` 复用精确项目/版本/文件的官方校验，并额外要求整合包项目类型与文件大小。准备任务先预留共享暂存额度，再传输到匿名 FD；下载后重新核对官方文件证据。具名 `Source::Local` 仍重新打开真实路径，`Source::Official` 仅绑定不透明来源 ID 和持有的已校验 FD，不借路径字符串伪造来源。短期输入缓存支持改名和可选项重检；关闭页面移除 pending ownership，迟到的下载不能重新插入。确认和已提交工作线程保留独立引用及额度，缓存关闭不会撤销已领取权限。共享 FD 的克隆会共享游标，官方输入的 ZIP/哈希读取统一通过 reader 锁串行化。官方证据在确认、取得队列 turn 和发布前复核，网络请求不占用 `operations` 锁。前端 `officialPackSource.ts` 把输入准备完成与正式安装完成分开，原 API 负责释放迟到结果；`official_tests.rs` 覆盖传输、目录与生命周期边界。

`mrpack/service.rs` 将检查后的源文件描述符、物理目标身份、可选项、网络策略和提供者解析结果放入有界确认缓存。界面得到一次性不透明 token，不得到可写的安装授权结构。缓存同时限制记录数、内存与实际持有描述符；领取后由工作线程持有租约，排队任务不会因未领取记录的 TTL 而失效。修改名称、来源或选择必须重查；旧 ZIP 写入入口拒绝其他格式，不能拿预览 DTO 绕过原生确认。

`crates/install/src/resolved.rs` 在检查阶段固定 Mojang 描述、Fabric 配置或 Forge / NeoForge 安装器与依赖证据。执行复用这些证据，不重新选择目录项或镜像声明。游戏核心和整合包传输使用同一捕获的网络与下载策略。`mrpack/mirror.rs` 校验允许的声明及每次重定向；`transfer.rs` 依序尝试声明中的镜像，校验大小与两种哈希，取消和丢弃异步任务释放请求、连接许可和匿名文件。失败镜像实际收到的字节也计入网络进度。

工作线程获得根目录队列 turn 后，先重查源内容、物理目标与启动器名称引用，再通过 `build.rs::BuildOperation` 创建 schema 2 的 Building 记录。私有根身份持久登记后才交给安装器的 `install_resolved_request_bound`。`crates/install/src/bound_root.rs` 持有根与最终父目录 FD；路径仅作为 `/proc/<工作线程 tid>/fd` 的系统调用适配，不能重新 canonicalize 为可替换的目录名。单独工作线程的 Landlock 限制 Java 自行派生的写入路径，缺少 ABI 3 / openat2 时在写入前失败；全局网络 runtime 在限制前初始化，避免影响其他任务。Java 临时文件也放在私有根，参数引用的 FD 保留到子进程结束。这是路径重定向约束，不提供对同一用户其他进程的完整隔离；并发外部硬链接仍可能共享文件 inode。归档链接被拒绝，执行的处理器来自捕获的官方安装器，整合包覆盖在处理器结束后应用。普通路径安装 API 保持原行为。所有核心和游戏内容先在私有根完成，`seal` 检查完整树与输出身份；`publication.rs` 负责共享文件相同内容复用、逐文件所有权、实例最终移动和恢复。schema 1 的旧 ZIP journal 保持兼容。构建暂存不按文件数量长时间持有大量 FD，封存后的输出按目录 FD 和文件快照重新打开。

提交回调在长哈希后短暂持有 `operations`，重检来源路径、物理目标与启动器引用并关闭取消接纳；已经进入提交的窗口关闭不撤销提交权限。Building / Prepared 恢复清理仍属本操作的输出，Committed 保留实例并清理暂存。活跃构建只豁免其精确 operation ID，其他未恢复或损坏记录继续阻止写入。异常、校验冲突和清理失败不能归类为普通取消。

排查格式误判看 `formats_tests.rs`；解析和覆盖看 `mrpack/tests.rs`；镜像、哈希与请求生命周期看 `mirror/tests.rs`、`transfer/tests.rs`；确认、队列和实际发布看 `service_tests.rs`；构建、提交与故障恢复看 `build_tests.rs`。前端保留目录/API/请求所有权，迟到的检查响应不能覆盖已编辑或关闭的表单。

`ResourceDetails` 只选择项目、版本和精确文件名；`ResourceInstall` 读取并确认绑定实例的方案。客户端提交的 URL、哈希或加载器不能取得写入权限。`resource_install_commands` 将原生准备的方案放入 `modrinth_install/confirmation.rs` 的有界内存缓存，返回一次性不透明 revision。开始安装时领取 `ConfirmedInstall` 并绑定工作线程；未领取的记录会过期，已提交请求不受缓存淘汰或排队时长影响。网络和大型本地哈希检查不持有 `operations`。任务提交即保留目标队列位置；获得目录 turn 后重检绑定与确认方案再开始下载，页面导航与目录浏览不改变捕获目标。

确认复用保持原实例物理身份、完整核心继承链、旧库存的身份/内容/启禁状态，以及官方版本、文件类型、文件名、URL、大小、哈希和依赖图。原先需要下载的文件可以变为相同已启用内容的复用；原先复用的文件不能变为新下载。新增库存必须经官方识别与文件类型检查；新增的已启用资源再做兼容与正反依赖检查，不能用同哈希的资源包冒充 Mod 前置。普通准备和排队复核共享反向精确依赖检查，重新确认不能绕过冲突。传输与提交使用新捕获的完整 target，不用旧库存快照发布。

`modrinth_install/provider.rs` 处理官方 API、响应限额和请求取消；`plan.rs` 处理必要依赖、兼容与本地哈希冲突；`confirmation.rs` 管理原生确认及允许的库存新增；`target.rs` 固定实例继承描述、资源目录身份及文件快照；`transfer.rs` 接收并校验文件。不同目录的工作线程共享匿名暂存父目录，创建竞争只接受 `EEXIST` 后严格重新打开，仍拒绝链接、普通文件和挂载替换。取消会丢弃正在等待的请求和匿名文件描述符。计时与取消等待使用既有 Tokio runtime；网络字节仅来自收到的响应，不把复用或磁盘复制计为速度。

所有新文件验证后才交给 `resource_ops::import_verified_batch`。它把模组、资源包和光影当作一个事务，提交前只写自己的暂存区，避免提前改变规划中的资源目录。桌面提交回调关闭取消接纳，再重读已接纳的取消标记，检查目标与旧资源事务；不能使用包含当前批次的 guard 拒绝自己的 journal。

持久状态为 Staging → Prepared → Committed。提交前的恢复只清理仍匹配登记 inode 与内容的本次输出；提交后保留安装结果并清理暂存。根目录级恢复入口不依赖所选实例或某一个资源类型，使跨类型的部分提交仍能恢复。冲突和清理失败保持 Error 与记录，不被迟来的取消掩盖。排查入口为 `modrinth_install/tests.rs`、`resource_ops/verified_batch/tests.rs` 及资源命令的任务集成用例。

### 本地模组更新与恢复

`LocalResources.tsx` 持有列表、筛选和本地文件操作；`useResourceUpdates.ts` 持有更新检测、历史与请求作用域，`ResourceUpdates.tsx` 显示确认方案。检测和确认响应必须属于当前根目录、实例与请求世代；客户端只提交旧文件名称、扫描指纹和确认 revision。

`modrinth_install/updates/` 按 SHA-512 识别已装项目，并通过官方批量接口获取正式版候选。文件日期按 RFC3339 时间值比较，不能使用字符串顺序判断升级。确认时重新解析多项目依赖图，保留原目标快照；替换权限由已识别文件与新图推导，不能通过删掉旧库存来绕过已装项目的固定前置版本或不兼容约束。原首次安装路径仍拒绝覆盖。

`resource_update_commands.rs` 从信息检查起持有 `ResourceUpdate` 单写入任务；撤销使用 `ResourceUpdateRestore`。网络字节只计真实响应；恢复步骤没有下载阶段。提交回调先关闭取消接纳，再检查已接纳的 token 和完整原库存，最后检查已捕获目录与其他事务。普通依赖、身份、哈希或回滚错误不被迟来的取消变成 Cancelled。

`resource_ops/update_batch/` 将恢复记录放在物理实例内，按目录身份和隔离规则定位资源，避免永久历史依赖旧实例名称。准备阶段复制并校验备份，不改变旧资源指纹；开始替换后把原文件 inode 移入恢复区，再发布新文件。原 inode 的保留使连续更新的撤销能够依次返回先前状态。根目录待恢复标记防止实例被外部移走后隐藏未完成事务。

持久状态为 Staging → Prepared → Applying → Committed；撤销为 UndoPrepared → UndoApplying → Restored。提交前只清理本次暂存；Applying 恢复到原文件，Committed 保留新文件与原文件历史。撤销中断恢复到可重试的已更新状态。发布、回滚与撤销都核对已登记的 inode、内容和目录身份，遇到外部冲突保留记录。完整日志只在状态边界保存，每个文件的所有权使用小记录登记，避免大型列表反复写入整份 manifest。

## 修改时维持的约束

- **明确目标和所有权**：命令捕获 `root_id`、规范目录和实例 ID；后台任务不随当前页面重新选择目标。删除或清理只处理已登记且仍匹配的文件。
- **文件操作与资料同步分清提交边界**：改变 journal 状态顺序、fsync、原子发布或 marker 清理顺序时，必须重新检查各崩溃窗口的恢复行为。反例用例放在改名模块的 `tests.rs` 中。
- **进程与文件锁的生命周期**：Java 探测可能与后台文件操作并行。`flock` 由打开的文件描述共享，复制或 fork 后仅关闭父进程的描述符不足以保证解锁；锁守卫应在工作结束时显式 `LOCK_UN`，仍保留非阻塞互斥。见 [Linux flock 文档](https://man7.org/linux/man-pages/man2/flock.2.html)。重置与改名的重复描述符用例覆盖这一边界，避免只依赖线程调度复现偶发占用。
- **注释说明原因**：记录不可破坏的约束、锁顺序、失败/恢复策略以及不明显的平台限制。普通语句用清楚的命名表达；过长流程先按职责提取函数。
- **避免含糊的组合值**：有关联的计划、引用和 revision 使用具名结构；错误和副作用应在调用处可见。
- **变更说明和回归用例一起维护**：深层 bug 的用例应覆盖导致故障的时序或数据状态；仅复述实现步骤的测试不能证明恢复正确。

## 验证入口

```sh
# 完整 Rust 工作区，包含桌面命令与应用集成。
cargo test --workspace --locked --features pcl-desktop/custom-protocol

# 改名文件事务及引用迁移的定向用例。
cargo test --locked -p pcl-desktop --features custom-protocol instance_rename

# Java 探测、策略及登记边界。
cargo test --locked -p pcl-core -p pcl-desktop --features pcl-desktop/custom-protocol java

# 类型检查与前端生产构建。
npm run build --prefix apps/desktop

# 格式检查。
cargo fmt --all -- --check
```

改名文件事务用例在 `apps/desktop/src-tauri/src/instance_rename/tests.rs`，引用迁移用例在 `instance_rename_refs/tests.rs`，设置迁移用例在 `config/tests.rs`，Java 核心用例在 `crates/core/src/java_tests.rs`，应用集成用例在桌面 `main.rs`。它们使用独立样例目录；不要把个人实例路径或账号数据写入公开测试。

Rust 测试不能证明真实桌面选择器、网络授权页面或 Minecraft 启动正常。涉及这些边界时，还需要按[使用指南](USAGE.md)在专用实例中手动验证。

## 启动器偏好与桌面服务

启动器偏好独立于 `config.rs` 的游戏配置。`launcher_prefs/model.rs` 定义强类型字段、局部 patch 和范围检查；`launcher_prefs.rs` 管理 revision；`launcher_document.rs` 为偏好和社区收藏提供各自独立文件、锁与限额的原子保存；`launcher_prefs/transfer.rs` 显式列举备份字段，避免将未来的运行状态或秘密意外导出。格式不受支持或外部修改时保留文件，不把缓存当作重新读到的配置。

`launcher_commands.rs` 在提交前准备网络客户端与下载调度器，再持准入提交设置和交换快照。新任务捕获不可变快照，不能在运行中反复读取全局偏好。窗口效果有独立串行顺序，使用最新设置；不要跨窗口管理器调用持有游戏操作锁。

| 服务 | 入口 | 关键边界 |
| --- | --- | --- |
| 代理与解析 | `crates/network/` | 环境代理快照、DoH 限额与缓存、系统解析退回、保持 TLS 验证 |
| 下载策略 | `crates/network/src/download.rs` | 异步与阻塞读取共享并发槽和字节预算；取消释放等待；元数据不节流 |
| 媒体与主页 | `launcher_assets/`、`launcher_asset_commands.rs` | 只向渲染器开放已登记的媒体 ID；主页只读受限 JSON，远程地址检查与请求地址固定 |
| 游戏日志 | `launcher_logs/`、`launcher_log_commands.rs` | 有界读取、脱敏导出、排除当前日志、清理和恢复记录所有权 |
| 统计、诊断、应用入口 | `launcher_local/`、`launcher_runtime.rs` | 统计按实际事件递增；诊断使用固定事件类型；只移除拥有且未修改的入口 |
| 公告 | `plugins/home/` | 官方插件拥有固定项目发行来源、缓存和请求同步，`launcher_discovery/` 保留兼容检查入口 |
| 剪贴板发现 | `plugins/clipboard/`、`launcher_discovery_commands.rs` | 提供者拥有 GTK 读取与链接识别；命令绑定开关修订及激活，读前和异步回调复核原生焦点；WebUI effect 位于 `src/plugins/discovery/` |
| Minecraft 版本提醒 | `launcher_minecraft_updates/`、`launcher_minecraft_update_commands.rs`、`useLauncherMinecraftNotices.ts` | 官方版本身份与发布时间、独立已读记录、检查与确认的生命周期 |
| 程序更新 | `launcher_updates/`、`launcher_update_commands.rs` | 官方发行资产验证、运行版本与磁盘版本分开、可恢复交换和回退 |

前端 `useLauncherPreferences` 负责偏好写入与回复接纳，其他 `useLauncher*` hook 各自拥有媒体、桌面操作、更新和发现请求的生命周期。用户切页、改变策略或导入设置后，旧请求不能执行后续自动下载、展示旧主页或切换资源详情。

## 启动准备与依赖完整性

`launcher_preparation` 固定一次启动的 root、实例、设置与偏好修订、关闭代次及 monitor generation。core 的 `inspect_files_with_isolation` 只检查启动文件，不探测 Java；健康实例直接构建原有 plan，不初始化网络客户端或进入缺失补全的目录图约束。

存在缺失时，`MissingRepair` 从固定 root FD 重新捕获下载依据，`repairable_missing_count` 与执行共用 URL 和继承 JAR 资格。只有可补全项才准入全局 `InstanceRepair` 写租约；慢校验、下载和 plan 重建均在操作锁外。租约覆盖重建及最终核验，任务取消 token 与 monitor 的本次准备绑定，新的准备代次清除旧 token。任务终态后，spawn authorization 同时固定操作与 monitor 准入，跨有界 ACK 交接后释放，再执行原有 adoption；已接受的取消不能继续生成游戏进程。

`IntegrityCheck` 是独立只读 API，不能提交给缺失补全写入器。已有资源索引通过捕获的 SHA1 与大小后才能扩展对象清单；损坏或缺少依据的索引只产生未知覆盖提示。文件读取使用固定 FD，按块检查取消与内存代次，读前后及报告返回前核对文件身份与 stamp。`InstanceVerify` 任务提供关闭取消与排他租约，报告只描述已枚举文件，成功完成不代表已修复；完整结果在任务页保留，显式关闭后才收起。

## 独立游戏监控

所有窗口可见性模式共用 `launcher_game_monitor/`。相同可执行文件的监控入口先于 GUI 与配置初始化运行；启动参数通过有界匿名管道交接，不写入监控记录或命令行。监控进程拥有 Java、输出管道和日志寿命，GUI 只观察状态与提交停止请求。

持久记录绑定进程 ID、启动身份、游戏根和日志位置。停止必须重新检查身份，不能只向缓存 PID 发送信号。损坏或未知记录保留并阻止写入；记录消失不证明游戏已经退出。重新打开 GUI 时，`launcher_monitor_runtime.rs` 将实际监控状态接入准入和运行状态。

schema 2 记录显式实例 ID，schema 1 不从文件名补猜身份。schema 3 增加日志字节起点：监控打开追加日志后、启动游戏前持久保存，并随 READY 交接给 GUI。起点属于同一会话的不可变身份；历史 schema 不补猜起点，原有追加日志保留。恢复的实例只有与其游戏根和监控会话一起使用才有意义。已确认结束的历史记录删除或被可解析记录替换时，退役旧报告与日志绑定，不重新把该游戏标成运行中；仍在运行时丢失记录则保留原来的占用保护。新启动准备通过准入后，旧终态记录会在生命周期锁与修订校验下持久标记为 `retired`，保留日志，避免准备失败后的再次重开恢复过期历史。

排查退出、重新启动与窗口恢复交叉问题时，核对会话所有权：旧任务的终态、停止意图和可见性恢复不能作用于下一次启动。成功交接后仍需消费准备阶段收到的取消请求，再执行隐藏或关闭。

`launcher_failure_commands.rs` 接收显式 root 和界面所见的失败状态，`launcher_failure.rs` 在操作锁内捕获监控代次、设置修订及日志目录/文件身份。日志读取和已知账号字段脱敏在锁外进行，返回前重新验证整个绑定。新一轮准备启动会清除旧日志引用；没有本次日志时不搜索“最新日志”作为替代。

`launcher_logs::FailureLogCapture` 从固定文件描述符读取最多 256 KiB、400 行尾部，以本次启动起点为下界；只有字节上限切断首行时才丢弃它。目录或文件替换使报告失效，起点越过文件末尾则拒绝读取。没有起点的历史日志标记为 `history`，分类器只使用本次失败消息，不使用历史尾部。`launcher_failure_analysis.rs` 是纯文本分类器，只接收已脱敏文本，返回有限错误族与有界证据，未匹配和单纯退出码保持未知。这些线索不代表确定根因或模组兼容性结论。

`launcher_failure::PreparedDiagnostic` 将监控代次、设置与任务修订、文件身份、日志起点及最终脱敏文本绑定为内容修订。导出重新读取并核对修订，再调用 TXT 保存选择器；选择器与慢读不持操作锁，最终发布持锁复核，使用匿名暂存和无覆盖发布，拒绝在当前游戏目录中保存。报告只包括已加载文本，不查找其他崩溃报告替代。

`launcher_failure_ai` 的预览和发送均从原生报告构造载荷，界面不能传入报告字节、密钥或目标地址。最多 48,000 UTF-8 字节，另受 64,000 字节 JSON 输入预算约束；需要摘录时明确标记开头、末尾和日志范围。Node 私有 `diagnostic_*` 通道与普通任务入口隔离，一次性确认令牌绑定报告和捕获的完整预设，10 分钟失效。预览不调用模型，发送消费令牌后使用冻结的目标与限制。

诊断使用独立 `diagnostic` profile，运行时强制空工具集；输出仅以纯文本展示，不进入 RH Code 项目历史。状态和取消按报告／任务身份查询，不重新读取可能已变化的日志。前端关闭、禁用或切换范围后退役旧回复，并取消已经准入或迟返确认的任务。

前端 `LaunchFailure` 将报告和操作绑定到失败状态、目录/API 与导航代次，退休后的回复不能重新打开旧报告。进入日志页使用一次性的精确文件名选择；列表中缺失时明确显示不可用，刷新不会静默选另一份日志，用户手动选择优先于已经消费的入口请求。

`LauncherLogContent` 只接收面板已经读取的文本，不持有原生 API。每次被接纳的读取都有独立内容代次，切换目录、API 或日志使旧查找失效。`launcherLogSearch` 对转义后的字面关键词按原始 Unicode 字符串索引匹配，避免大小写转换改变偏移；最多 256 个 UTF-16 单位输入、1,000 个高亮节点，额外一个匹配只用于确认截断。导航复用匹配结果，先在所属内容面板内露出查找栏与日志，再定位高亮；不会滚动窗口或转移输入焦点。

实例游戏日志由独立的 `instance_logs` 只读服务处理，`instance_log_commands` 负责原生命令边界。目录选择沿用 core 的内容隔离规则；列表修订绑定实例、配置与目录，文件修订绑定用户选中的载体，读取在操作锁外进行，发布前重新验证。只接受固定 `logs`／`crash-reports` 子目录内的支持格式，不接受任意路径。压缩输入、解压输出、文本尾部和列表规模均有上限；读取文本经已有账号字段脱敏后才返回界面。

`InstanceLogs` 保留显式文件选择，刷新、切换实例／目录／API、关闭与准入变化都会让旧请求失效。已加载文本交给 `LauncherLogContent` 搜索。它不推断报告与某次启动的关联，也不改变 `LaunchFailure` 的精确日志绑定。

实例日志导出复用同一读取流程，`content_revision` 绑定列表、文件及脱敏后实际显示的文本。原生服务重新读取并核对该修订，持有所选文件和目录，再请求保存位置；选择器不持有操作锁。发布复用已有匿名暂存与无覆盖原语，最后持准入锁复核源目录、配置、任务代次、文件及目标父目录。游戏目录内的目标拒绝保存；原始压缩字节和未加载文本不参与导出。

## 单文件保存与更新事务

任务终态的界面同步有两条职责：`taskNeedsBootstrap` 更新实例、目录与恢复投影，`taskNeedsResourceScan` 仅推进当前目录的本地资源扫描代次。原生任务进度修订不作为资源列表的依赖，避免每次进度轮询都重新扫描。目录选择器或目录操作持有界面 busy 时，bootstrap 请求保留为待刷新；操作在 `finally` 解锁后消费，读取仍通过 `stateRequest` 隔离迟返结果。

`resource_save_commands.rs` 持有选择器返回的目标路径和短期确认 token；客户端只能提交官方项目、版本、精确文件名。`resource_save/authority.rs` 重读官方元数据并生成 revision，`files.rs` 固定目录描述符、校验匿名文件并执行无覆盖发布。传输前和提交前都复核同一份文件权威信息。

保存的取消边界为：匿名下载与校验 → 目标／权威信息复核 → 任务关闭取消接纳 → 再检查已接受的取消 → 原子发布。迟到取消不能掩盖哈希或路径错误；发布之后的目录同步问题作为成功附带警告，不能声称文件不存在，也不能删除后来被外部修改的内容。

启动器自身更新采用不同事务：`launcher_updates` 校验发行清单、架构、可执行格式和完整哈希，交换程序前持久登记，再以文件身份核对恢复。准备阶段回退原程序，已提交阶段保留新程序并提供回退；运行中的程序不因磁盘替换而成为新版本。安装任务的准入由实际阻塞工作线程持有，调用方异步 future 消失不提前释放它。

## 收藏、工具箱与只读信息导出

`launcher_document.rs` 只支持两种固定文档：偏好与资源收藏。它们共享目录描述符、外部编辑检测和原子交换代码，各自使用独立文件名、写锁与大小限额。默认加载不创建文件；收藏保存只接纳原生提供者确认的身份和介绍，不接纳客户端 URL、标题或图标。批量收藏先确认完整项目集合，再以一个视图 revision 发布整份变更，缺失或无关项目不能导致部分保存。

`ToolboxDownload.tsx` 负责输入与确认，`toolbox_download/authority.rs` 持有系统选择器得到的目录和短期 token。实际 URL 只保留在原生内存；确认展示去掉查询参数。`transfer.rs` 检查每次重定向、完整响应、大小与超时，并使用提交时的网络和下载策略快照。`files.rs` 固定目录身份、复核匿名文件和无覆盖发布；任务持有准入直到实际工作线程退出。

原生关闭入口 `begin_launcher_close` 使用相同的 `operations` 准入锁：即使当前空闲，也先设置关闭状态，再捕获活动任务。取消和等待在释放锁后进行，避免检查空闲与新任务接纳之间存在窗口，也避免等待线程阻塞任务的收尾提交。

「停止使用」关闭失败时，只能撤销自己仍持有的关闭请求。`close_generation` 在准入锁下赋予请求所有权，后来的原生关闭不能被旧失败回复撤销。被动 `launcher_closing` 事件带同一 generation；渲染器拒绝乱序旧事件，不接管原生窗口销毁或任务等待。

### Minecraft 版本提醒的检查与确认

`launcher_minecraft_updates/provider.rs` 只读取固定 Mojang 版本清单，在大小、类型、身份与时间检查后选择各类型最新条目；不读取游戏目录，也不安装版本。`receipt.rs` 使用独立的固定文件、锁和容量限制，保存每个类型的发布时间高位、待提示版本和有界已读身份。第一次启用建立安静基线；清单回退、同一身份的元数据变化不能倒退或推进记录。

命令在 `operations` 下捕获开关、不可变网络快照和策略 epoch，释放锁后等待网络，收尾时重新核对它们。相关开关、网络、设置导入/重新读取与关闭使旧请求失效；普通外观变化不改变策略。检查成功可以持久保存待提示状态，但不能标为已读；当前前端接纳合并提示后才提交短期 token，原生再次核对策略和记录 revision。忽略的旧回复不会吞掉通知，确认失败保留待提示状态。渲染器单次会话记住已显示身份，重试确认时不重复显示；显示与落盘之间的崩溃允许重启后再次提示。

`ToolboxGenerators.tsx` 与 `achievementImage.ts` 使用真实内置图像生成成就 PNG。`toolbox_images/pixels.rs` 负责有界 PNG 解码、Minecraft 头部坐标、透明叠加与最近邻缩放；选择的皮肤以原生内存快照和过期 ID 绑定，不把源路径交给渲染器。图片保存前重新验证编码，选择器取消和发布冲突均保留已有内容。

`local_resource_info` 使用描述符绑定所选实例、隔离规则与精确扫描指纹，先限制 ZIP 索引，再读取有界元数据条目。`ui_data.rs` 将列表展示和信息导出分为不同解析用途：列表可截取文字，导出保留原字符串和资源包的结构化介绍。大量哈希读取不持有操作锁，选择器结束后重新检查内容；最后的短期准入保护目标绑定和文件发布。它不调用路径重开的列表接口，也不提取归档内容或序列化账号信息。

## 多任务协调

`Tasks::admit` 保持旧事务的全局立即独占语义。四种网络任务显式调用 `admit_queued`，声明游戏根目录或已验证的最终输出文件 scope；禁止用显示用 root ID 代替物理路径互斥。`TaskScope` 核对规范路径、目录 inode 和祖先关系，同根/嵌套根排队，不相关路径可以越过阻塞的队首。最多4项运行、32项等待。

工作线程在 operations 锁外调用 `wait_turn`，获得 turn 后重检目录登记、名称保留、恢复记录及方案 revision。取消只设置该任务的 token 并唤醒等待；任务 owner 释放暂存和捕获的文件描述符后才能报告终态、释放 scope。关闭入口捕获全部运行与等待任务，取消和 drain 在 admission 锁外执行。bootstrap 对恢复 journal 的抑制只查询实际运行的 workers，不能让 queued 任务遮住恢复问题。

`download_tasks` 返回整个任务集合、单调修订号和同一快照下的 `blockedRootIds`。事件只唤醒集合读取，避免乱序事件覆盖新状态；`download_status(taskId)` 和取消结果按明确 ID 返回，旧无参数调用仅用于兼容。前端导航、轮询、取消都由独立的 owner/任务 ID 控制，一项结束不能退出其他尚未结束的任务。网络策略仅在集合空闲时交换，防止不同 Arc 快照产生多份总速度预算。

Rust 的 `tasks/events.rs` 仅将同一任务、阶段和可取消状态的进度通知限制到每 100 毫秒一次；标题、阶段和终态变化立即发布，调度器仍保存每次更新。WebUI 将短时事件合并为一次权威读取，有活动任务或订阅失败时每秒兜底，原生订阅正常且空闲时每 15 秒兜底。订阅建立后、窗口重新获得焦点以及显式提交或取消操作都会要求新读取。相同任务修订可复用原任务对象，但根占用投影与任务限额仍须独立核对。

`statusPolling.ts` 为进程与账号状态分别保留一个物理读取槽，等待上次请求结束后再计时。空闲周期为 15 秒，准备或运行游戏时为 900 毫秒，账号登录或保存时为 1 秒；操作完成与窗口返回会主动刷新。公开字段全部相同时复用原 App 数据，关闭与替换后的迟到回复不得覆盖新状态。相关回归入口为 `scripts/status-polling.test.mjs`、`scripts/task-polling.test.mjs` 与 Rust `tasks_scope` 测试。

## 实验功能接入

`apps/desktop/src-tauri/src/experimental.rs` 管理一个独占的本地 Node 子进程，固定命令枚举通过有界 JSON 行通信。应用关闭先停止任务和受管 Python 子进程；传输损坏后丢弃会话，避免把旧回复当作下一命令的结果。项目路径来自宿主，实例摘要在 Rust 内投影，第三方声明不能获得完整 `Api` 或 bootstrap。

`experimental/host.mjs` 是轻量 stdio 与插件接线入口，首次请求 AI 服务时才导入 `agent-host.mjs`。两层共用一个 `ExtensionStore`，插件卡片请求不会初始化 AI 引擎。共享运行时在 `experimental/runtime/src/`，确定性的 Python 工具在 `vendor/maker`、`vendor/porter`。`SOURCES.json` 记录集成版本与当前发布的工具源码哈希；修改工具后运行 `node experimental/scripts/sync-sources.mjs`，发布前用 `--check` 检查，组件版本须单独同步。引擎管理工程任务、产物哈希与修改确认，模型只能提出修改；授权和应用仍由宿主 UI 调用。制作器恢复和模板更新通过宿主选择的工程副本进入源码编辑器；模型的源码编辑工具只接受原有补丁结构，不接受恢复路径。

无活动会话和退役进程时，原生宿主可在同一会话锁下为固定插槽、无过滤器的卡片读取及目录读取返回空投影，前提是保留目录句柄、权限和名字复核证明插件记录确实不存在，且没有另一宿主持有现有锁。此路径不创建目录或锁文件、不启动 Node，也不授权操作。任何现有记录都交给真实插件服务处理；损坏记录的卡片和目录读取会报告 `STORE_UNREADABLE`，保留原文件与安全模式。

Node 候选全部确实缺失时才返回 `PCL_NODE_MISSING:`；成功版本探测得到不支持的旧版本时返回 `PCL_NODE_UNSUPPORTED:`。权限、坏链接、进程失败及无效版本输出不会降级成这两类提示。界面只识别固定前缀，重试仍受原页面、请求与控制器归属约束；普通引擎后台刷新保持原生命周期。

`ExperimentalTools` 分发制作器与搬运工作区，`ExperimentalMaker` 负责物品表单，`ExperimentalProjects` 管理制作器产物登记与项目说明，`ExperimentalProjectFiles` 提供只读文件索引。`ExperimentalPorterWorkspace` 管理持续搬运项目与讨论，`ExperimentalExtensions` 负责授权与纯文本卡片，`ExperimentalAi` 负责预设与共享引擎状态；它们使用现有 CE 控件与中英消息目录。异步结果只更新所属页面和选择；页面退役不撤销已提交的宿主事务。

制作器项目存储在 `vendor/maker/projects.mjs`，搬运项目存储在 `porter-projects.mjs`；两个格式独立，共用运行时、源码产物和 AI 预设。项目版本引用宿主持有的任务产物，不接受界面提供的源码目录权限。`artifact-snapshot.mjs` 统一检查产物收据、输出目录与文件指纹；`vendor/maker/source-index.mjs` 用有界流式读取验证完整清单，再返回文件页、文本片段或搜索页。较大的只读索引不扩大源码编辑与 AI 续作的快照限额。

搬运轮次冻结源码基线、目标和讨论上下文；运行期间的新反馈用于下一轮。`porter.ask_user` 先持久化问题，再结束当前模型循环并释放运行名额。用户答复后另起轮次，模型不能借此审批补丁。`runtime/src/reviews.mjs` 将转换方案的未支持事项和验证限度纳入确认摘要，应用只生成新的文本副本。恢复、损坏记录与外部修改的处理见项目存储及其回归用例。

数据型插件宿主在 `experimental/extensions/src/`。存储按声明内容摘要绑定授权；损坏时保留原文件并停用。外部 N/Nex 清单只产生无安装令牌的诊断报告；`extensions/compat/` 的 N SDK 探针是独立开发验证工具，启动器尚未开放第三方程序集加载。

实验数据位于项目配置目录下的 `experimental/`。`ai-presets.mjs` 持久化预设、密钥和工作限制，保存使用权限 0600、文件快照修订号与原子替换；损坏、未知版本或外部修改保留原文件并拒绝写入。界面只接收模型配置和密钥是否存在，不接收已保存密钥。`pi-selection.mjs` 与 `pi-limits.mjs` 分别处理路由与限制，`pi-live.mjs` 在异步加载 SDK 前捕获每个任务的配置。停止引擎取消活动任务但保留预设；修改预设只影响后续任务。常规安装与游戏目录事务仍使用原有 Rust 服务。实验工具不自动执行生成工程的构建脚本。

前端兼容性优先面向 Chromium；Linux 桌面容器继续使用 Tauri 的 WebKitGTK。新增界面应使用通用浏览器 API，原生编辑等平台行为保持在 Rust 适配层。

## 名称与兼容性

显示名称和项目仓库使用 PCL RH。既有账号密钥环服务、Tauri 应用标识、本地存储目录与插件格式标识仍沿用原来的稳定标识，避免产品改名造成数据或授权丢失。桌面入口的读取同时识别旧名称和当前名称的精确模板；外部编辑或其他项目的入口仍不属于本项目。

### CurseForge 本地整合包文件解析

本地 CF ZIP 的清单保留精确 project/file ID，只有已配置的官方文件服务可将其转为下载依据。项目类型决定内容目录，官方 API 检查归属、许可、可用状态、URL、SHA1 和大小；归档不能自行授予 CDN 地址。整个清单解析成功后才更新确认内容和修订，工作线程在构建和发布前重新核对。服务未配置或文件被限制时保留阻止项，不跳过缺少的文件。

CF 下载使用独立来源分支，仅接受允许的官方文件节点之间至多 5 次、无循环的 HTTPS 重定向；官方 SHA1 校验后另算本地 SHA512。API 与文件请求使用原生端捕获的凭据，文件鉴权限于明确允许的官方 HTTPS `/files/` 地址。Key 只放在敏感 `x-api-key` 请求头，不进入 URL、下载计划、回执或 WebUI。Modrinth 原有双哈希、镜像和重定向策略保持各自语义；本地清单跨站重定向重新构造请求，不携带 CF 鉴权。此入口只负责本地 CF ZIP，不扩展社区详情直接安装或 MCBBS 中的 CF 引用。

### CurseForge 发行构建

官方构建从 GitHub Actions 仓库 Secret `PCL_RH_CURSEFORGE_API_KEY` 注入项目自己的服务凭据。`build_credentials.rs` 将它写到 Cargo `OUT_DIR`，由 Rust 原生端编译使用，不写入源码、前端资源或 Cargo 日志指令。官方包的使用者无需申请或填写 Key。

运行时的同名环境变量优先于内置值，供开发者覆盖。自行构建也可设置 `PCL_RH_CURSEFORGE_API_KEY_FILE` 指向自己的私有文件；构建时环境变量优先于文件。未提供凭据的源码构建仍可使用其他资源服务，CF 入口会报告未配置。无效覆盖不会静默回退到官方凭据。维护者应将私有文件置于版本管理之外，限制文件访问权限。

`Official native candidates` 工作流仅由官方仓库 `master` 手动触发，复用 `desktop-platforms.yml` 在四个原生主机生成 `release` 配置的未签名候选包。官方 CF 凭据只显式传给 Cargo 构建步骤，npm、运行检查、清单和日志不接收凭据值。普通 `Desktop platform builds` 继续构建不含官方凭据的预览。

统一打包器只取 HEAD 中白名单生产资源及显式二进制、运行依赖，不导出整个源码树或预建用户状态。解压验证会直接运行桌面程序的 `--build-info`，在项目、账号、网络和 WebView 初始化前取得固定编译信息，核对版本、提交、目标平台、Cargo 配置、`custom-protocol` 和内置 CF 可用状态；CLI 版本独立核对。打包器不执行输入程序，清单中的 `binary_source_commit` 仍保持未知；编译信息的检查结果单独出现在候选验证报告中，不冒充签名或来源认证。流程不创建 tag、发布 Release 或更新本机安装。

`tools/installer-native.py` 从已核验的 Windows 便携包生成 Inno Setup 当前用户安装器。每个资源按清单独立列出并绑定 SHA256，拒绝 Windows 路径别名、指令插入和用户状态目录，不递归复制源码树。卸载使用安装器自己的文件日志，保留运行后产生的用户数据。主 EXE、CLI 优先于其他资源安装，使被占用的主程序尽早阻止升级；这不是任意晚失败可回滚的事务。`tools/ci/verify-native-installer.py` 仅限临时 Windows runner，验证实际安装、快捷方式、占用失败、解锁重装及保留数据的卸载。

`windows-installer.yml` 可复用官方候选中已验证的 Windows 便携产物，核对来源运行、提交和 artifact 摘要后才处理。`PCL_NATIVE_SOURCE_COMMIT` 表示被验证程序的来源，工作流自己的 `GITHUB_SHA` 不变；安装报告分别记录程序与工具的提交，避免把较新的验收工具误当成二进制来源。快捷方式通过只读 `IShellLinkW` 接口取得完整 Unicode 路径，再核对目标和工作目录的实际文件身份；输出使用 ASCII 转义 JSON，避免控制台代码页损坏路径。

`windows-window-smoke.py` 只观察新建子进程自己的可见原生窗口，确认稳定显示后向同一窗口发送 `WM_CLOSE`，要求正常退出。它不操作其他应用，也不将窗口创建当成 React 页面、登录或游戏功能通过的证据。

正式 Windows 桌面程序使用 GUI subsystem，CLI 保留 console。辅助模式在路径和 WebView 初始化前分流，候选检查读取真实 PE Subsystem，并用显式管道运行 `--build-info` 和拒绝无效构建请求；独立 monitor 夹具也是真实 GUI PE。普通启动的致命错误由 `startup_failure` 输出详细诊断并显示固定原生提示，不把私密路径或未知输入放入弹窗。

## 多文件下载并发

`pcl-network::for_each_bounded` 轮询有界、借用的 transfer futures，不创建脱离提交者的异步任务。Java 与整合包将已校验的匿名 FD 在每个完成回调中立即暂存；普通资源批量下载使用同一收集器保留待事务导入的文件。出现错误或取消时，所有在途响应、FD 与连接许可先释放，外层才清理暂存。全局连接数与总字节节奏仍由捕获的 `DownloadScheduler` 统一管理；元数据读取和按依赖排序的加载器处理器不变成无序并发。

原版支持库／资源文件及加载器支持库使用有界工作线程，传输仍取得同一调度器的许可。并行进度必须累计每个文件的增量；单个文件的局部进度不能覆盖整批计数。文件发布继续要求完整大小与摘要校验，不使用未经验证的部分文件。

## 自动内存分配

`core/src/memory.rs` 只负责模式类型和纯预算算法；输入为总内存、可用内存与启用模组数，输出为整数 GiB 与受预算限制标志。算法为系统／原生分配保留余量，同时限制小实例的默认堆；不会探测 Java、修改配置或申请实际内存。

`config.rs` schema3 的 `memory_mode` 缺省 Manual，`memory_mode_overrides` 以登记根和实例 ID 为范围。显式模式优先；缺少模式但存在旧数值覆盖时保持 Manual；都缺少时继承全局。切换 Auto 不清空手动值，继承则清除两个实例覆盖。模式引用参加改名迁移与名称保留；v2 journal 序列格式保持原样。

`launcher_memory.rs` 在工作线程中读取 Linux `/proc/meminfo`，按捕获的隔离策略从固定目录 FD 统计启用的普通 JAR，不读取模组内容。符号链接、无法读取的目录或扫描期间的替换不能变成零模组估算。Manual 完全跳过系统和游戏目录探测。`memory_preview` 发布前核设置修订、登记根与关闭代次；前端 `MemoryControl` 将回复绑定 root／实例／revision，分配量、警告与内存条使用同一次预估快照。

`main.rs::launch_game` 在 `PreparedLaunch::prepare_with` 最终构建闭包内重新求值，再交给原生计划的 `-Xmx`。启动检查使用同一服务，自动估算失败时报告不可启动；没有保存或回退到旧的手动值。页面预估不是后续启动的已确认额度。
