# 插件使用与声明格式

顶部橙色入口打开插件管理。内置模块可以按依赖启停；「社区插件」接收 JSON 声明或带摘要的 `.pclext.json` 包。社区插件目前提供纯文本卡片、页面跳转和经授权的实例摘要；内置组件开发见[RH Code 插件开发](RH_CODE_PLUGINS.md)。商店暂未开放。

## 安装与管理

1. 在「社区插件」点击「导入插件」，选择文件。
2. 确认名称、版本、作用范围和权限；勾选所需权限后安装。必需权限未同意时不会安装。
3. 使用「启用」「停用」保留或隐藏插件卡片；查看权限可以撤销授权。撤销必需权限后插件暂停，重新授权前不会显示卡片。
4. 「卸载」会移除该插件的声明和授权。之后重新导入需要重新确认权限。

状态变化会刷新已经打开的首页、工具卡片和 RH Code 卡片。保存失败时保留原状态。安全模式暂停所有社区卡片，关闭后恢复；安全模式和插件状态在重启后保留。

PCL N / Nex 文件仍只提供兼容性报告，不会变成可执行插件。

## 作用范围

| `scope` | 卡片位置 | 展示时机 |
| --- | --- | --- |
| `launcher` | `home.secondary`、`tools.cards` | 启动器首页或插件管理预览 |
| `rh-code` | `rh-code.cards` | RH Code 的「插件卡片」区域 |
| `both` | 上述任意位置 | 对应页面打开时 |

旧声明省略 `scope` 时仍属于启动器，原有授权摘要保持不变。范围不会自动授予权限。RH Code 中可在任一区域的内容选择器选「插件卡片」，也可以拆分、移动或关闭该区域。内置「社区插件卡片」模块可以单独停用，不会卸载已安装的社区声明。

仅展示卡片时使用轻量插件宿主；AI 引擎在需要其功能时才加载。社区声明不加载 JavaScript、程序集、任意网页或 Shell。

## 编写一个卡片

将下面内容保存为 `workspace-note.json`，随后在启动器中导入：

```json
{
  "format": "pcl-linux.declarative-extension",
  "schemaVersion": 1,
  "id": "local.workspace-note",
  "name": "Workspace Note",
  "version": "1.0.0",
  "publisher": "Local author",
  "scope": "rh-code",
  "api": { "min": 1, "maxExclusive": 2 },
  "capabilities": [
    { "id": "ui.cards", "required": true, "reason": "显示工作区提示卡片。" }
  ],
  "contributions": {
    "cards": [{
      "id": "note",
      "slot": "rh-code.cards",
      "title": "本轮目标",
      "text": "修改前确认游戏版本与加载器。",
      "actions": []
    }]
  }
}
```

`format` 中的旧名称是兼容标识。修改已安装声明的内容时需要提高三段版本号，再导入确认；不会自动沿用新增权限。已有例子见[工作区提示](../experimental/extensions/examples/workspace-notes.json)与[实例指南](../experimental/extensions/examples/instance-compass.json)，编辑器结构提示见[JSON Schema](../experimental/extensions/schema/manifest.schema.json)。

声明为 UTF-8 JSON，完整文件不超过 64 KiB，最多 12 张卡片，每张最多 4 个操作。ID、文字、重复字段、API 版本和权限会由运行时校验；声明不接受 HTML 或自定义 CSS。

## 卡片操作

| `kind` | 所需权限 | 其他字段 |
| --- | --- | --- |
| `navigate` | `launcher.navigate` | `target` 为 `launch`、`instances`、`downloads`、`tools` 或 `settings` |
| `show-instance-summary` | `instances.summary` | 不接受目标路径；使用启动器当前选择 |

所有声明都需要必需的 `ui.cards` 权限。实例摘要仅包含游戏版本、加载器、模组数量和隔离状态，不包含实例名、文件路径或账号。没有选中实例或缺少操作权限时，相应按钮不可用。

打包工具可通过 `experimental/extensions/src/package.mjs` 的 `createPackage(source)` 生成带 SHA-256 摘要的 JSON 包。摘要用于发现内容不匹配，不验证发布者身份。
