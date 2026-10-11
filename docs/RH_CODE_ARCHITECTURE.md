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
| `code/source-ownership.mjs` | 在任务副本中协调手写源码保护，保持原文件夹不变 |
| `apps/desktop/src-tauri/src/rh_code.rs` | 项目存储、修订检查、插件依赖、对话归属和原生源码快照 |
| `rh_code/files.rs` | 有界读取、内容指纹、原子保存和空目录产物发布 |
| `rh_code/layout.rs` | 布局及组件状态校验、可移植预设投影 |
| `experimental/host.mjs` | 既有 Pi 执行、预设、审阅、任务产物和兼容接口 |

表中缩写路径分别相对于 `apps/desktop/src/` 和 `apps/desktop/src-tauri/src/`。

## 状态与写入顺序

`RHCodeHost.tsx` 位于启动器的页面切换节点之外，持有稳定的插件内核与工作区生命周期。初始启动只读取插件清单；所有当前模块都声明为 RH Code 范围，所以首次进入前无需导入工作区或启动其数据服务。内核按作用范围导入入口，工作区休眠时仅释放 RH Code 实例，启动器和通用实例继续存活。

可见性变更与休眠通过同一串行队列协调。离开 10 分钟且活动结束 5 分钟后，宿主先检查后台引擎，再由控制器保存草稿、布局及所有项目的源码缓冲。`prepareSuspend` 在每次异步保存后复核编辑代次和可见性；只有最后同步检查仍成立才封闭旧控制器并释放缓存。活动任务不受静默时长影响。

组件的临时表单可使用 `ViewHost.keepAwake()`，完成或保存后释放；视图卸载会回收其剩余租约。插件自身的异步工作使用 `PluginRuntime.keepAwake`，并将释放函数注册到 Cordis effect。前端实例释放不等于清除浏览器模块缓存。后台 `experimental_suspend` 通过原有会话锁检查准入请求和 Node 工作拥有者，空闲时回收子进程，后续调用重新启动。

项目元数据保存在启动器状态目录的 `rh-code/workspace.json`，使用版本化 schema。Rust 在读取和写入期间持有进程锁与文件锁，写入比较调用方的内容修订号，再进行原子替换。损坏或更新版本的文件会保留，不能以空项目列表覆盖它。

WebUI 控制器串行提交元数据，始终使用最近成功的原生修订。待保存布局按项目保留，快速切换项目不会恢复旧的几何状态。异步区域回调绑定其原项目；同一文件的较晚读取不能覆盖已有编辑缓冲区。

区域保存视图选择、定位引用及按视图 ID 分区的 JSON 状态；关闭或停用区域不会释放项目数据。插件用 Cordis effect 注册组件、命令和布局，卸载时清理自己的注册及依赖模块。已取出的命令引用在其注册退役后也会拒绝执行。

布局预设沿用 schema 1，以默认空数组兼容旧文件。原生层限制树深、区域数量、标识符和状态大小，未知组件 ID 可保留。预设在完整验证后移除项目定位和组件状态，套用时生成新区域 ID。具体边界见[内置插件开发](RH_CODE_PLUGINS.md)。

Agent 准入分三步：Rust 捕获当前对话和源码；宿主创建未启动任务；Rust 持久保存用户消息与活动任务绑定。只有绑定成功才允许启动模型。任务结束时按精确 job ID 追加回执，重复完成不会重复写入，也不能清除另一个活动任务。

现有审阅服务验证产物清单和内容哈希。采用源码时，原生桥取得经过验证的字节，在相邻暂存目录构建，再一次发布到空项目目录。渲染层不能自行提交字节或指定任意任务取得写入权限。

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
cargo test --locked -p pcl-desktop --features custom-protocol rh_code
```

这些检查覆盖真实 Cordis 卸载与恢复、布局身份、原生存储冲突、对话任务归属和源码保护。它们使用隔离数据；不证明真实模型服务、Java 构建或游戏运行已经通过。
