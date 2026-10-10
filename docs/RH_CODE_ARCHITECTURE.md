# RH Code 模块结构

RH Code 将项目数据、可替换视图和 Agent 执行分开。阅读代码时先追踪数据的持有者，再追踪插件注册和任务回执，避免把区域显示状态当作项目或任务的生命周期。

## 入口与职责

| 入口 | 职责 |
| --- | --- |
| `apps/desktop/src/rh-code/RHCode.tsx` | 工作区外壳、项目切换、设置及旧版入口 |
| `rh-code/controller.tsx` | 项目服务、串行写入、草稿与文件缓冲区、任务对话绑定及完成同步 |
| `rh-code/kernel.ts` | Cordis 插件实例、依赖排序、视图与命令注册、卸载清理 |
| `rh-code/plugins/index.ts` | 内置插件到视图和 Agent 命令的实现映射 |
| `rh-code/plugins/` | 对话、源码、图谱、构建目标、审阅和输出视图 |
| `rh-code/layout.ts` | 无副作用的拆分树操作，保持区域身份 |
| `rh-code/Workspace.tsx` | 渲染拆分树、分隔线和区域操作 |
| `code/plugins.json` | WebUI 与 Rust 共用的内置清单、依赖和视图归属 |
| `code/agent-context.mjs` | 完整对话到有界模型上下文的投影 |
| `code/agent-adapter.mjs` | Rust 快照到独立 Pi 任务的适配，先准备后启动 |
| `code/source-ownership.mjs` | 在任务副本中协调手写源码保护，保持原文件夹不变 |
| `apps/desktop/src-tauri/src/rh_code.rs` | 项目存储、修订检查、插件依赖、对话归属和原生源码快照 |
| `rh_code/files.rs` | 有界读取、内容指纹、原子保存和空目录产物发布 |
| `experimental/host.mjs` | 既有 Pi 执行、预设、审阅、任务产物和兼容接口 |

表中缩写路径分别相对于 `apps/desktop/src/` 和 `apps/desktop/src-tauri/src/`。

## 状态与写入顺序

项目元数据保存在启动器状态目录的 `rh-code/workspace.json`，使用版本化 schema。Rust 在读取和写入期间持有进程锁与文件锁，写入比较调用方的内容修订号，再进行原子替换。损坏或更新版本的文件会保留，不能以空项目列表覆盖它。

WebUI 控制器串行提交元数据，始终使用最近成功的原生修订。区域仅保存视图选择与定位引用；关闭或停用区域不会释放项目数据。插件用 Cordis effect 注册视图和命令，卸载时清理自己的注册。

Agent 准入分三步：Rust 捕获当前对话和源码；宿主创建未启动任务；Rust 持久保存用户消息与活动任务绑定。只有绑定成功才允许启动模型。任务结束时按精确 job ID 追加回执，重复完成不会重复写入，也不能清除另一个活动任务。

现有审阅服务验证产物清单和内容哈希。采用源码时，原生桥取得经过验证的字节，在相邻暂存目录构建，再一次发布到空项目目录。渲染层不能自行提交字节或指定任意任务取得写入权限。

## 扩展边界

新增内置视图先在共用清单声明依赖，再在实现映射注册。可释放的订阅和命令使用 Cordis effect，数据保存在项目服务中。不要通过组件卸载隐式删除项目或取消任务。

社区插件继续经过独立的声明与原生授权服务；内置插件代码属于可信发布内容。Cordis 提供依赖和生命周期，不提供第三方代码隔离。

目前 RH Code 使用 DSH 的 Cordis 包，并借鉴其提示词分区、任务调度及上下文投影理念。Pi 仍是执行适配器，没有替换为完整 DSH Agent SDK。模型上下文投影保留原始历史，不伪造摘要或已读内容。[DSH 架构](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md)提供上游背景；Cordis 的 MIT 许可保留于 [LICENSES/CORDIS.txt](../LICENSES/CORDIS.txt)。

## 独立检查

```sh
node apps/desktop/scripts/check-rh-code.mjs
node --test code/tests/*.test.mjs
cargo test --locked -p pcl-desktop --features custom-protocol rh_code
```

这些检查覆盖真实 Cordis 卸载与恢复、布局身份、原生存储冲突、对话任务归属和源码保护。它们使用隔离数据；不证明真实模型服务、Java 构建或游戏运行已经通过。
