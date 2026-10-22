# 内置产品插件开发

内置插件与 PCL RH 一起编译发布。现有对话、源码、图谱、目标和输出组件已经有各自的插件入口；组件选择器和布局菜单读取注册表。社区入口接收带范围标签的声明型插件，可管理启停、授权与卸载，不能加载这些 TypeScript 模块；格式见[社区插件指南](PLUGINS.md)。

启动器与 RH Code 共用 `code/plugins.json` 和顶部橙色 Plugin 管理入口。插件可以提供 WebUI 组件、原生服务或 AI 工具；这些实现分别由对应运行时执行，共用插件身份、启停配置与作用范围。

## 启动器原生插件

| 插件 ID | 实际实现 | 原生接口 |
| --- | --- | --- |
| `launcher.home.announcements` | 首页公告组件、请求状态，以及原生读取、缓存与同步 | `launcher.announcements@1` |
| `launcher.diagnostics.analysis` | 启动失败的规则匹配与线索生成 | `launcher.failure-analysis@1` |

在同一个 Plugin 管理页可停用、重新启用它们。停用公告插件后撤回公告及旧操作回调；停用分析插件后不接受新的预分析，原有日志报告、导出与 AI 分析入口仍可使用。游戏进程与已有日志不会被删除或停止。已接纳的调用可完成，服务资源在最后一个调用结束后释放。

共用产品运行时与服务契约位于 `apps/desktop/src-tauri/src/product_plugins/`，原生宿主位于 `plugin_host/`，实际实现位于相邻的 `plugins/`。服务按接口名称、版本与 Rust 类型登记；初始化失败不会发布部分服务。依赖先于消费者启动，停用时先撤销消费者的新调用。消费者按契约查找实现，不写死内置提供者 ID，同时保留原有页面与命令协议。当前要求同一契约只有一个活跃提供者；多个提供者同时活跃会明确报错，不按加载顺序选择。日志捕获、脱敏、导出与 AI 准入仍经过原有事务校验，不由诊断规则直接访问文件。

这两个插件是随应用编译的可信 Rust 实现。社区 JSON 导入不能加载 Rust 动态库或任意脚本；当前也未提供第三方原生二进制插件安装接口。首页其他行为与其他启动器服务仍沿用现有实现。

`nativeServices` 声明原生接口及版本；`runtimeNeeds` 声明 `native` 或 `node` 运行时依赖。纯原生启动器插件不会阻止闲置的 RH Code 及 AI Node 宿主休眠。旧清单省略 `runtimeNeeds` 时保留原先的保守行为。

## 添加一个组件

### 启动器组件

启动器组件通过 `launcherSurfaces` 贡献到明确的显示位置，与 RH Code 的 `views` 分开。当前提供 `home.announcements`：清单需为 `launcher` 或 `both` 范围，声明 `launcher.announcements.read` 能力及 `{ "id": "插件组件ID", "slot": "home.announcements" }`。每个清单最多声明 32 个启动器组件，ID 必须唯一。

入口返回的 `ProductPlugin.launcherSurfaces` 使用 `{ id, owner: manifest.id, slot, render }`，必须与清单一致。参考 `launcher.home.announcements.plugin.tsx` 及 `src/plugins/home/AnnouncementSurface.tsx`。首页只指定位置；组件与请求状态由插件持有，只有激活成功的贡献才会显示。

`render` 接收 `LauncherSurfaceProps`。`host.announcements` 只提供公告读取、打开发布页、显示策略、语言与通知；组件不接收通用原生命令或项目控制器。`lifetime.isActive()` 标识当前挂载，提交操作前和接纳异步结果时都应检查。停用、依赖退役、宿主替换或关闭后，旧生命周期不会因重新启用而恢复。组件渲染异常会隔离该插件的界面贡献，可在原有 Plugin 管理入口重试。

这些是随包发布的可信代码贡献，不是 JavaScript 沙箱，也不赋予社区 JSON 插件加载脚本的能力。启动器组件不创建第二个 Kernel；RH Code 休眠不会卸下仍然启用的启动器贡献。

### RH Code 区域组件

1. 在 `code/plugins.json` 添加清单，声明唯一 ID、运行范围、依赖和贡献 ID。
2. 在 `apps/desktop/src/rh-code/plugins/` 添加 `*.plugin.tsx`，默认导出 `BundledPluginEntry`。`rh.example` 使用 `example.plugin.tsx`；其他命名空间使用完整 ID，例如 `launcher.example.plugin.tsx`。
3. 启动开发界面或重新构建。启用插件后，它的组件自动出现在区域选择器中。

清单示例：

```json
{
  "id": "rh.example",
  "name": "Example",
  "version": "1.0.0",
  "required": false,
  "scope": "rh-code",
  "dependencies": ["rh.project"],
  "capabilities": ["project.metadata"],
  "views": ["rh.example.summary"]
}
```

对应 `example.plugin.tsx`：

```tsx
import { Blocks } from "lucide-react";
import type { BundledPluginEntry } from "../kernel";
import type { ViewProps } from "../types";

function Summary({ api }: ViewProps) {
  return <div className="rc-panel-pad">{api.project?.name}</div>;
}

const entry: BundledPluginEntry = {
  id: "rh.example",
  create: (manifest) => ({
    manifest,
    views: [{
      id: "rh.example.summary",
      owner: manifest.id,
      label: "项目摘要",
      icon: Blocks,
      tone: "blue",
      render: Summary,
    }],
  }),
};
export default entry;
```

完整的可设置组件参考 `overview.plugin.tsx`。面向用户的标签使用 `locale.ts` 的 `label(中文, English)`，样式使用 `--rc-*` 变量和工作区组件类。

## 贡献类型

| 贡献 | 清单字段 | 实现 |
| --- | --- | --- |
| 启动器组件 | `launcherSurfaces` | `{ id, owner, slot, render }` |
| 区域组件 | `views` | `{ id, owner, label, icon, tone?, render, settings? }` |
| 命令 | `commands` | `{ id, owner, handler }` |
| 布局 | `layouts` | `{ id, owner, label, create(project) }` |
| AI 工具 | `tools` | Node 宿主实现；声明 `{ name, capability, modes }` |

贡献 ID 必须在自己的清单中声明，不能占用另一插件的 ID。标识符以 ASCII 字母或数字开头，可使用字母、数字、点、下划线与连字符，最长 120 字符。清单由装配器复制冻结；工厂返回收到的同一个 `manifest`。

## 添加一个内置 AI 工具

AI 工具使用单独的 Node 登记边界。工具名为小写命名空间与名称，例如 `example.inspect`，最长 80 字符。所属插件需在 `capabilities` 中声明对应能力，再添加：

```json
"tools": [
  {
    "name": "example.inspect",
    "capability": "project.metadata",
    "modes": ["ask", "plan", "execute"]
  }
]
```

在随应用发布的宿主初始化模块中，使用 `HarnessToolCatalog.forPlugin("rh.example")` 登记实现。工具对象包含相同的 `owner`、`capability`、`modes`，以及 `name`、`description`、`inputSchema`、`validate(args)`、`replaySafety` 和 `execute(args, context)`。当前内置工具可参考 `experimental/runtime/src/harness-tools.mjs`；AI 宿主装配入口是 `experimental/agent-host.mjs`，由轻量入口 `experimental/host.mjs` 按需导入。

`replaySafety` 支持只读、幂等或需人工协调。询问模式的工具必须只读。参数校验、文件范围、取消检查和结果边界由实现负责，能力声明本身不执行这些检查。需要紧凑回执的只读或幂等工具可提供 `projectReceipt(result)` 和 `replayCompleted: true`；重放必须验证原快照或已提交的不可变操作版本，重建原结果，不能再次修改数据。

新任务保存具体工具名、归属、版本、能力和模式。之后增加工具不会扩大旧任务清单；归属或版本发生变化时，旧任务不能悄悄改用新声明继续运行。`retire(owner)` 移除该插件的工具注册，保留的旧函数也会拒绝执行；整个目录用 `dispose()` 释放。停用界面模块会影响后续任务的准入，已运行任务保留捕获清单。

这个接口用于随应用发布、经过维护者审查的内置实现。社区扩展的 JSON、卡片或权限声明不能注册 JavaScript 工具；Cordis 与工具目录都不提供第三方代码沙箱。

## 视觉工具与预览

`rh.pixel` 和 `rh.model` 是完整的组件与 AI 工具实例，分别声明 `project.assets.pixel` 和 `project.assets.model`。前端使用共享的 `code/visual-assets.mjs` 校验、编辑与渲染文档；Node 的 `VisualService` 将相同操作应用到任务私有版本。像素及模型工具各有 `create`、`edit`、`inspect`、`propose`，其中 `inspect` 可用于只读模式，其他工具要求执行模式和已经记录的计划。

编辑以有界、原子的结构化批次完成。工具返回文档摘要和真实 PNG；Pi 适配器仅向支持图片的模型发送图片内容块，不把 Base64 当作文字。任务记录只保存版本与图片指纹等紧凑回执，重放验证同一不可变版本并重建图片。预览不授予项目写入权限：`propose` 仍调用通用 Harness 建议服务，用户通过原有审阅入口应用 JSON 文档。

手动保存与导出经过 Rust 的 `asset_save` / `asset_export`：重新验证文档、插件状态和输出路径；原生编码 PNG 和 Java 模型资源，并使用可恢复事务发布。它们是用户编辑器操作，不接受 AI 自动批准。二进制导出不绕过文本建议的审阅边界。

## 组件外观与状态

`tone` 是组件的可选语义色：`blue` 用于文件与图谱，`purple` 用于 AI 对话，`orange` 用于构建、迁移与审阅，省略时为 `neutral`。宿主负责深浅主题、标题栏与焦点样式，组件移动后仍保留自己的色彩身份。

`render` 与可选 `settings` 都接收 `{ area, api }`：

- `api.project`：当前项目。按只读数据使用；修改项目要调用宿主命令。
- `api.state`：当前区域、当前组件的 JSON 对象。
- `api.updateState(next)`：替换该组件的状态，保留同区域内其他组件的状态。
- `api.invoke<T>(id, ...args)`：调用当前已注册命令；返回类型由命令约定。

当组件含尚未持久保存的本地编辑或后台工作时，可调用 `api.keepAwake()` 获取防休眠租约，完成后调用返回的释放函数。组件卸载会释放该次挂载尚未释放的租约。需要跨区域挂载的插件后台工作可使用工厂收到的 `runtime.keepAwake?.()`；插件必须用 `ctx.effect` 登记释放函数，让卸载撤回租约。租约让 RH Code 控制器在离开工作区后继续完成必要工作，不保存组件状态。

状态由宿主持久保存，组件切换或插件停用后仍保留。设置按钮只在组件提供 `settings` 时显示。区域关闭会删除该区域的设置；对话、源码缓冲区和任务由项目服务持有，不能只放进组件状态。

每个区域的全部组件状态上限为 64 KiB、嵌套深度 8。对象键最长 120 个 ASCII 字符，可包含字母、数字、点、下划线与连字符；禁止 `__proto__`、`constructor`、`prototype`。状态应保存轻量展示设置，凭据及项目文件应通过专门服务管理。

## 生命周期与依赖

清单的 `scope` 控制模块何时导入和实例何时存在：

| 范围 | 生命周期 |
| --- | --- |
| `launcher` | 启动器存续期间加载；退出 RH Code 后继续保留实例。 |
| `rh-code` | 进入 RH Code 后加载；工作区休眠时卸载，再次进入时创建新实例。 |
| `both` | 任一宿主范围活跃时加载；退出 RH Code 后保留启动器仍在使用的实例。 |

旧清单省略 `scope` 时按 `rh-code` 处理。插件管理页能显示未加载插件的清单与休眠状态；读取清单不会执行入口。停用的可选插件和依赖被阻塞的插件也不会导入。`launcher` 与 `both` 插件不能依赖仅在 RH Code 中存续的插件；这类清单会在导入前失败。

`create` 在对应范围活跃且依赖可用时装配贡献，`apply(ctx)` 处理需要清理的初始化。订阅、定时器和监听器使用 `ctx.effect` 返回清理函数。导入模块时不要启动后台工作。

```ts
apply(ctx) {
  ctx.effect(() => {
    const unsubscribe = service.subscribe(onChange);
    return () => unsubscribe();
  });
}
```

依赖未启用或加载失败时，插件处于阻塞状态。卸载先清理依赖方，再清理被依赖的模块，撤回组件、命令和布局，并释放宿主持有的工厂实例；已保存的组件定位不会被清除。浏览器仍缓存已导入的 ES 模块，重新进入时重新运行工厂，不重复执行模块顶层。初始化失败后可在插件页重试。普通命令失败由调用方显示错误，不会自动卸载整个插件。组件渲染失败可以局部重载。

内置代码属于可信应用代码。Cordis 的注册与生命周期检查不隔离 JavaScript；能力字符串也不授予原生文件访问。涉及文件、进程或项目事务的能力，需要经过 Rust 宿主接口及对应的原生校验。当前旧组件仍可使用内部控制器，新组件优先使用 `ViewHost`。

## 可共享布局

布局最多 12 个区域、深度 7；拆分比例在 0.15 至 0.85 之间。导入 JSON 最多 256 KiB，只接受 `pcl-rh-code-layout` 格式版本 1。未知组件 ID 被保留为占位。

项目布局可带文件和对话引用、组件状态；可共享预设只保留拆分树与组件 ID。导出经过完整校验再移除项目数据，套用时重新生成区域 ID。内置布局也由插件提供，参考 `settings.plugin.tsx`。

开发检查命令见[模块结构](RH_CODE_ARCHITECTURE.md#独立检查)。范围与延迟加载回归可在 `apps/desktop` 运行 `node scripts/check-rh-code-scopes.mjs`；该检查覆盖实际模块导入、共享实例保留、工作区退场与重入、依赖范围校验，以及导入或初始化期间关闭宿主的清理。

## 固定构建工具

`rh.build` 的视图、命令和工具共用同一插件身份，依赖必需的 `rh.project`。AI 的 `build.run` 额外检查本轮原生捕获的项目构建许可、Execute 模式及已完成计划；插件启用本身不授予执行许可。`build.read_log` 只读取本任务的运行日志。

模型只能选择 `javac` 或 `gradle` 和固定目标，可附带同任务的精确提案 ID/digest。接口不接受 shell、任意参数、工作目录、可执行路径或网络权限。执行器由 Rust 宿主提供，不能通过社区声明替换。构建回执和日志是执行证据，不能成为新权限或自动批准源码改动。

## 社区卡片区域

`rh.extensions` 是一个可停用的可信内置模块，贡献 `rh.extensions.cards` 视图，读取社区宿主的 `rh-code.cards` 插槽。社区声明提供文字、宿主渲染的设置控件和固定操作，不会取得 `ViewHost`、项目控制器或原生 IPC。`PluginRuntime.navigate` 由产品宿主提供，只接受固定页面目标；加载与休眠仍遵循内核范围。

社区声明和授权由独立 `ExtensionStore` 保存，不写入仅允许内置 ID 的 `disabledPlugins`。状态变更发布无载荷的前端通知，各插槽立即撤回旧卡片并重新读取；已保留的旧按钮不继续导航。工作区卡片请求是只读活动，不把轮询算作新的编辑。

声明型组件的类型、持久数据与授权操作见[插件规范 v1](PLUGIN_SPEC.md)。新增原生作用必须同时经过 Node 的能力检查和 Rust 的类型化消费者，不能从社区字段选择原生命令。

## 数据驱动的动态组件

可信内置插件可声明受限的动态视图命名空间，供运行时数据生成组件。下面是添加到内置清单的字段：

```json
"dynamicViews": {"prefix": "rh.example.panel.", "limit": 16}
```

`prefix` 必须位于自身插件 ID 的子命名空间、以点结尾，最长 80 个 ASCII 字符；`limit` 是 1～384 的整数。动态描述符仍由随应用编译的代码创建，不接受社区提供渲染回调。

`apply(ctx, services)` 的第二个参数提供 `services.replaceViews(views)`：

- 一次提交完整的新视图集合。宿主先检查整批归属、命名空间、数量、描述符和冲突，再替换；失败保留上一批。
- 每项仍是 `ViewContribution`，`owner` 必须是本插件 ID；不能占用其他插件或任何静态声明的视图 ID。
- 用 `ctx.effect` 绑定数据订阅，并在返回的清理函数中取消订阅、退役未完成请求。停用、依赖退役、重试或释放后，旧发布函数不能再次注册视图。
- 更新标题或状态时保留未变组件的 `render` 函数身份，避免让 React 卸载表单而丢失草稿。

`Kernel.listViews()` 给选择器提供静态元数据与当前动态组件；静态模块休眠不保留它的渲染闭包。未知的社区面板 ID 保留为可恢复的布局占位。

可信 `rh.extensions` 模块就是这一接口的使用例：通过轻量目录端口订阅社区元数据，以固定受控渲染器创建独立面板。社区声明不会因此得到内置命名空间、`services`、原生接口或运行任意代码的权限。
