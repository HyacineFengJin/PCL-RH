# RH Code 内置插件开发

内置插件与 PCL RH 一起编译发布。现有对话、源码、图谱、目标和输出组件已经有各自的插件入口；组件选择器和布局菜单读取注册表。社区入口目前仍只接收声明型插件，不能加载这些 TypeScript 模块。

## 添加一个组件

1. 在 `code/plugins.json` 添加清单，声明唯一 ID、依赖和贡献 ID。
2. 在 `apps/desktop/src/rh-code/plugins/` 添加 `*.plugin.tsx`，默认导出 `BundledPluginEntry`。
3. 启动开发界面或重新构建。启用插件后，它的组件自动出现在区域选择器中。

清单示例：

```json
{
  "id": "rh.example",
  "name": "Example",
  "version": "1.0.0",
  "required": false,
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
| 区域组件 | `views` | `{ id, owner, label, icon, tone?, render, settings? }` |
| 命令 | `commands` | `{ id, owner, handler }` |
| 布局 | `layouts` | `{ id, owner, label, create(project) }` |

贡献 ID 必须在自己的清单中声明，不能占用另一插件的 ID。标识符以 ASCII 字母或数字开头，可使用字母、数字、点、下划线与连字符，最长 120 字符。清单由装配器复制冻结；工厂返回收到的同一个 `manifest`。

`tone` 是组件的可选语义色：`blue` 用于文件与图谱，`purple` 用于 AI 对话，`orange` 用于构建、迁移与审阅，省略时为 `neutral`。宿主负责深浅主题、标题栏与焦点样式，组件移动后仍保留自己的色彩身份。

`render` 与可选 `settings` 都接收 `{ area, api }`：

- `api.project`：当前项目。按只读数据使用；修改项目要调用宿主命令。
- `api.state`：当前区域、当前组件的 JSON 对象。
- `api.updateState(next)`：替换该组件的状态，保留同区域内其他组件的状态。
- `api.invoke<T>(id, ...args)`：调用当前已注册命令；返回类型由命令约定。

状态由宿主持久保存，组件切换或插件停用后仍保留。设置按钮只在组件提供 `settings` 时显示。区域关闭会删除该区域的设置；对话、源码缓冲区和任务由项目服务持有，不能只放进组件状态。

每个区域的全部组件状态上限为 64 KiB、嵌套深度 8。对象键最长 120 个 ASCII 字符，可包含字母、数字、点、下划线与连字符；禁止 `__proto__`、`constructor`、`prototype`。状态应保存轻量展示设置，凭据及项目文件应通过专门服务管理。

## 生命周期与依赖

`create` 装配贡献，`apply(ctx)` 处理需要清理的初始化。订阅、定时器和监听器使用 `ctx.effect` 返回清理函数。导入模块时不要启动后台工作。

```ts
apply(ctx) {
  ctx.effect(() => {
    const unsubscribe = service.subscribe(onChange);
    return () => unsubscribe();
  });
}
```

依赖未启用或加载失败时，插件处于阻塞状态。卸载按依赖顺序撤回组件、命令和布局；已保存的组件定位不会被清除。初始化失败后可在插件页重试。普通命令失败由调用方显示错误，不会自动卸载整个插件。组件渲染失败可以局部重载。

内置代码属于可信应用代码。Cordis 的注册与生命周期检查不隔离 JavaScript；能力字符串也不授予原生文件访问。涉及文件、进程或项目事务的能力，需要经过 Rust 宿主接口及对应的原生校验。当前旧组件仍可使用内部控制器，新组件优先使用 `ViewHost`。

## 可共享布局

布局最多 12 个区域、深度 7；拆分比例在 0.15 至 0.85 之间。导入 JSON 最多 256 KiB，只接受 `pcl-rh-code-layout` 格式版本 1。未知组件 ID 被保留为占位。

项目布局可带文件和对话引用、组件状态；可共享预设只保留拆分树与组件 ID。导出经过完整校验再移除项目数据，套用时重新生成区域 ID。内置布局也由插件提供，参考 `settings.plugin.tsx`。

开发检查命令见[模块结构](RH_CODE_ARCHITECTURE.md#独立检查)。
