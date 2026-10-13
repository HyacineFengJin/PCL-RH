# RH Code 模块结构

RH Code 将项目数据、可替换视图和 Agent 执行分开。阅读代码时先追踪数据的持有者，再追踪插件注册和任务回执，避免把区域显示状态当作项目或任务的生命周期。

## 入口与职责

| 入口 | 职责 |
| --- | --- |
| `apps/desktop/src/rh-code/RHCode.tsx` | 工作区外壳、项目切换、设置及旧版入口 |
| `rh-code/controller.tsx` | 项目服务、串行写入、草稿与文件缓冲区、任务对话绑定及完成同步 |
| `rh-code/kernel.ts` | Cordis 插件实例、贡献归属、依赖生命周期、错误状态与卸载清理 |
| `rh-code/plugins/index.ts`、`catalog.ts` | 发现编译时内置入口，按共用清单装配插件 |
| `rh-code/plugins/*.plugin.tsx` | 各插件自己的组件、命令、布局与初始化入口 |
| `rh-code/plugins/` | 对话、源码、图谱、构建目标、审阅和输出视图 |
| `rh-code/layout.ts` | 无副作用的拆分树操作，保持区域身份 |
| `rh-code/layoutPresets.ts`、`Layouts.tsx` | 有界的可移植布局格式、保存与导入导出界面 |
| `rh-code/Workspace.tsx` | 渲染拆分树、分隔线和区域操作 |
| `rh-code/ViewBoundary.tsx` | 隔离单个组件的渲染失败并提供重载 |
| `code/plugins.json` | WebUI 与 Rust 共用的内置清单、依赖和视图归属 |
| `code/agent-context.mjs` | 完整对话到有界模型上下文的投影 |
| `code/agent-adapter.mjs` | Rust 快照到独立 Pi 任务的适配，先准备后启动 |
| `code/harness-snapshot.mjs` | 冻结文本、私有清单、内容指纹及旧 Maker 锁的保留 |
| `code/history-snapshot.mjs` | 私有对话历史、项目说明、记要和引用文件的不可变快照 |
| `code/harness-tool-catalog.mjs` | 工具归属、精确准入清单、模式与注册退役 |
| `code/harness-service.mjs` | 按需读取、检索、计划、不可变建议与宿主审阅投影 |
| `code/visual-assets.mjs`、`visual-png.mjs` | 共用像素/模型格式、原子编辑、CPU 渲染与 PNG 编码 |
| `code/visual-service.mjs` | 任务私有的视觉草稿、不可变编辑版本、图片回执及建议 |
| `rh-code/visual-assets/` | 像素和立方体编辑器、所属项目缓冲、按需画布与 AI 预览 |
| `experimental/runtime/src/harness-tools.mjs` | 通用工具声明、参数边界和紧凑读取回执 |
| `experimental/runtime/src/visual-tools.mjs` | 可选视觉工具的结构化参数与图像回传 |
| `code/source-ownership.mjs` | 旧 Maker 工作流的源码所有权协调 |
| `apps/desktop/src-tauri/src/rh_code.rs` | 项目存储、修订检查、插件依赖、对话归属和原生源码快照 |
| `rh_code/files.rs` | 有界读取、内容指纹、原子保存和空目录产物发布 |
| `rh_code/harness.rs` | 原生快照绑定、受审阅建议应用、备份与中断恢复 |
| `rh_code/assets.rs`、`harness/manual_assets.rs` | 手动资产校验、PNG/Java 导出与共用原生发布事务 |
| `rh_code/context.rs` | 上下文字段边界、连续历史片段及原生传输预算 |
| `rh_code/layout.rs` | 布局及组件状态校验、可移植预设投影 |
| `experimental/host.mjs` | 既有 Pi 执行、预设、审阅、任务产物和兼容接口 |
| `experimental/job-snapshot.mjs` | 同一次读取生成任务状态与结果，避免跨代完成回执 |

表中缩写路径分别相对于 `apps/desktop/src/` 和 `apps/desktop/src-tauri/src/`。

## 状态与写入顺序

`RHCodeHost.tsx` 位于启动器的页面切换节点之外，持有稳定的插件内核与工作区生命周期。初始启动只读取插件清单；所有当前模块都声明为 RH Code 范围，所以首次进入前无需导入工作区或启动其数据服务。内核按作用范围导入入口，工作区休眠时仅释放 RH Code 实例，启动器和通用实例继续存活。

可见性变更与休眠通过同一串行队列协调。离开 10 分钟且活动结束 5 分钟后，宿主先检查后台引擎，再由控制器保存草稿、布局及所有项目的源码缓冲。`prepareSuspend` 在每次异步保存后复核编辑代次和可见性；只有最后同步检查仍成立才封闭旧控制器并释放缓存。活动任务不受静默时长影响。

组件的临时表单可使用 `ViewHost.keepAwake()`，完成或保存后释放；视图卸载会回收其剩余租约。插件自身的异步工作使用 `PluginRuntime.keepAwake`，并将释放函数注册到 Cordis effect。前端实例释放不等于清除浏览器模块缓存。后台 `experimental_suspend` 通过原有会话锁检查准入请求和 Node 工作拥有者，空闲时回收子进程，后续调用重新启动。

项目元数据保存在启动器状态目录的 `rh-code/workspace.json`，使用版本化 schema。Rust 在读取和写入期间持有进程锁与文件锁，写入比较调用方的内容修订号，再进行原子替换。损坏或更新版本的文件会保留，不能以空项目列表覆盖它。

WebUI 控制器串行提交元数据，始终使用最近成功的原生修订。待保存布局按项目保留，快速切换项目不会恢复旧的几何状态。异步区域回调绑定其原项目；同一文件的较晚读取不能覆盖已有编辑缓冲区。

区域保存视图选择、定位引用及按视图 ID 分区的 JSON 状态；关闭或停用区域不会释放项目数据。插件用 Cordis effect 注册组件、命令和布局，卸载时清理自己的注册及依赖模块。已取出的命令引用在其注册退役后也会拒绝执行。

布局预设沿用 schema 1，以默认空数组兼容旧文件。原生层限制树深、区域数量、标识符和状态大小，未知组件 ID 可保留。预设在完整验证后移除项目定位和组件状态，套用时生成新区域 ID。具体边界见[内置插件开发](RH_CODE_PLUGINS.md)。

Agent 准入先保存编辑缓冲，再由 Rust 捕获当前对话和源码。宿主创建未启动任务及私有快照，Rust 将目录身份和快照指纹绑定到 job ID；控制器随后持久保存用户消息与活动任务绑定。只有绑定成功才允许启动模型。任务结束时按精确 job ID 追加回执，重复完成不会重复写入，也不能清除另一个活动任务。

`harness` profile 通过模式过滤工具。询问只读；规划增加计划回执；执行必须先完成计划回执才能提出建议。初始上下文有界，完整文本存于任务的 `workspace/source`，不可变清单和建议存于宿主私有 control 路径。读取工具只持久保存路径、指纹和范围等紧凑回执，重放同一操作时重新验证冻结内容并重建结果，避免把源码全文重复写入任务记录。

项目说明与对话记要、引用和名称使用原生修订检查。控制器按所属项目/对话持有编辑草稿，回执只结算对应编辑代次；切换区域不会丢弃未保存内容。任务准入先保存当前对话元数据，新的未保存编辑会阻止本轮捕获。既有 schema 1 文件默认这些字段为空，读取不会重写旧文件。

`conversationHistory` 是带绝对起始索引、消息 ID 和遗漏范围的连续后缀。原生层保持完整单条内容，在 1 MB、1024 条及最终 24 MiB stdio 预算内选取。宿主将其与说明、记要和引用共同保存在 `control/harness-history.json`，摘要输入只保存指纹与受限投影。`fitHarnessInput` 在附加工具清单后计算整个输入的 JSON/UTF-8 大小，仅收缩投影，不修改请求、权限、描述符或原始记录。

历史读取和字面检索重新验证私有快照。`read_history` 的 `offset` 选择片段内的消息；长消息用 `limit: 1` 和返回的 `nextContentOffset` 继续读取，索引和消息身份保持原值。说明与记要按完整字段读取，引用路径过长时单独分页。模型输出与旧讨论均不能授权新工具或批准源码建议。

工具声明位于共用清单的 `tools` 字段。宿主登记实现时匹配 owner、capability 和 modes，新任务捕获精确 `toolGrants`；调度器按捕获清单和当前注册过滤。退役移除注册并撤回保留的旧函数引用，旧清理回调不能移除新注册。没有工具清单的历史任务仅保留原有五种工具，不因升级取得新贡献。前端插件生命周期与 Node 工具登记各自管理资源，共用清单身份；社区声明型扩展不能借此装载可执行工具。

对话与输出组件只显示受限的助手文本、工具状态和计划，不读取原始 provider state、推理或工具参数。完成的建议经私有 stdio 导出；渲染层只能提交审阅时的 proposal ID 和 digest，不能提交替换字节。Rust 再复核本机绑定、项目和对话归属及快照指纹，并在有备份的事务中发布文件。原生提交后宿主保存 applied 回执；两者之间的失败通过精确 digest 重试协调。

任务读取的公开状态、修订、结果和工具活动来自同一次磁盘快照。不要为生成 summary 再读一次任务：任务可能在两次读取之间完成，导致前端收到「已完成」却仍持有旧的空结果，进而过早结算对话。

当前 Linux 应用边界使用 FD 相对的 `openat2`、匿名暂存 inode、`renameat2` 与持久恢复日志。发布按文件原子完成，批次中断由日志恢复；外部修改或身份变化阻止自动恢复并保留备份。控制器将应用加入元数据修订队列，保留 I/O 期间新输入的编辑。原生准入拥有整个捕获或应用区间，包含 stdio 请求之间的时间，防止休眠或关闭过早释放宿主。

旧版审阅和空目录「采用源码」仍通过原有服务提供，未改变为通用自动合并。任意 Shell 和游戏测试不在通用工具列表中；固定构建工具由独立插件提供。

## 构建执行的所有权

`rh.build` 贡献构建视图、`build.open` 命令和 `build.run` / `build.read_log` 工具。项目的 `buildPolicy` 由 Rust 持久保存，旧项目默认关闭。`agent_context` 仅在 Execute 模式且插件启用时捕获策略和二进制输入；任务参数不能扩大权限。后续策略变更不修改已准入任务。

`build-snapshot.mjs` 验证并冻结二进制清单。`HarnessService.buildSnapshot` 允许正在运行的任务使用同一任务的精确提案 ID/digest，构建路径不放宽对外导出与源码应用的完成状态要求。`HarnessBuildService` 在 `build-runs/<runId>/source` 拼装副本；control 记录与原始冻结输入不挂载进执行环境。

`AgentRuntime` 在整个工具调用期间持有任务租约；构建服务不能重入获取该租约，也不写 `job.json`。服务独立持有构建记录、日志、产物和取消句柄。它先持久预留 operation → run 对应关系，再启动执行；重放只返回已有回执，丢失拥有者的运行显示 interrupted，不自动再执行。原有模板校验 `BuildService` 保持独立。

Rust 在启动 Node 宿主时提供自身可执行文件路径。`NativeBuildRunner` 只以 `--rh-code-build-runner` 启动无界面的辅助模式，通过受限 NDJSON 交换 started/output/finished。stdin 是拥有权租约：取消或 EOF 要求辅助进程先清理 sandbox，再发送最终回执。强制终止且清理未确认时不能返回成功。活动构建加入 idle/shutdown 检查；停止既有任务不依赖启动命令插件仍然注册。

`code_runner` 将固定 javac/Gradle 配方放入 bubblewrap PID、用户、挂载等命名空间，默认关闭网络。副本 source/home 使用已打开的目录 FD 挂载，系统工具只读；不暴露原项目、宿主 control、用户 HOME 或凭据。资源限制为单进程及部分目录限制，没有使用系统 cgroup，也不保证整个进程树的内存或磁盘总配额。

输出有字节预算，逐步写入私有日志，记录只确认已写前缀。日志读取按 UTF-8 字节分页，模型回执省略全文。成功后按固定路径收集产物并记录哈希；「打开产物」再验证哈希，Rust 复核项目/任务归属、运行 ID 和无链接的私有目录。构建成功与游戏验证状态分开，不能把命令退出码推广为整体模组可用。

## 视觉资产的所有权

手动绘图和建模使用控制器的文件缓冲，新文档以空的原文件指纹表示尚未创建；内容保存后复读确认并记录真实指纹。关闭区域不会丢弃文档，任务准入和休眠沿用统一的保存顺序。界面状态只保存路径、选择及视角，不持有整份资产。渲染由数据或视角变化触发，不创建常驻渲染循环。

AI 的视觉操作在任务私有目录中保存不可变版本及操作提交记录，不直接编辑手动缓冲。相同操作 ID 重放返回原提交；创建和修改先验证冻结源码、保护记录、插件权限和计划。`inspect` 可选择模型视角，图片与版本指纹进入回执；图片字节不进入任务日志。宿主按已完成视觉操作缓存有界预览，轮询空闲任务不重复渲染。

`propose` 将私有文档送入通用文本审阅流程。手动导出是另一明确入口：Rust 从严格校验的 JSON 生成 PNG 或 Java 模型/纹理对，先检查全部目标和目录身份，再逐个原子发布并记录可恢复日志。导出采用仅创建语义，已有路径不覆盖；保存资产文档采用原指纹检查。两种入口共用发布和恢复实现，但手动编辑不伪造 Agent 审阅授权。

## 扩展边界

新增内置插件先在共用清单声明依赖与贡献 ID，再添加独立的 `*.plugin.tsx` 入口。Vite 在编译时发现这些入口；区域选择器和布局菜单从贡献注册表生成。新的组件不需要修改工作区外壳或 Rust 的视图名称列表。可释放的订阅使用 Cordis effect，项目数据保存在项目服务中。不要通过组件卸载隐式删除项目或取消任务。

工厂和初始化失败只停用该插件及受影响依赖，状态可查询与重试。组件渲染错误由区域边界处理，其他区域继续工作。模块顶层代码在对应作用范围首次加载时运行，并由浏览器缓存；可释放的资源应放进工厂或 `apply`，使用 effect 归还。

社区插件继续经过独立的声明与原生授权服务；内置插件代码属于可信发布内容。Cordis 提供依赖和生命周期，不提供第三方代码隔离。

目前 RH Code 使用 DSH 的 Cordis 包，并借鉴其提示词分区、任务调度及上下文投影理念。Pi 仍是执行适配器，没有替换为完整 DSH Agent SDK。模型上下文投影保留原始历史，不伪造摘要或已读内容。[DSH 架构](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md)提供上游背景；Cordis 的 MIT 许可保留于 [LICENSES/CORDIS.txt](../LICENSES/CORDIS.txt)。

## 独立检查

```sh
node apps/desktop/scripts/check-rh-code.mjs
node apps/desktop/scripts/check-rh-code-presets.mjs
node apps/desktop/scripts/check-rh-code-controller.mjs
node apps/desktop/scripts/check-rh-code-host.mjs
node apps/desktop/scripts/check-rh-code-scopes.mjs
node --test experimental/runtime/tests/idle-host.test.mjs
node --test code/tests/*.test.mjs
node apps/desktop/scripts/check-rh-code-harness-ui.mjs
node apps/desktop/scripts/check-rh-code-visual-ui.mjs
node --test experimental/runtime/tests/visual-provider.test.mjs
node --test experimental/runtime/tests/harness-provider.test.mjs experimental/runtime/tests/harness-host.test.mjs
cargo test --locked -p pcl-desktop --features custom-protocol rh_code
cargo test --locked -p pcl-desktop --features custom-protocol experimental::tests
cargo test --locked -p pcl-desktop --features custom-protocol code_runner
```

这些检查覆盖 Cordis 卸载与恢复、布局身份、原生存储冲突、对话归属和源码保护。构建服务测试使用模拟 helper；原生 runner 的 Linux 检查使用隔离 Java 和 wrapper 夹具，依赖本机 JDK/bubblewrap。它们不证明真实模型服务、任意模组工程或游戏运行已经通过。
