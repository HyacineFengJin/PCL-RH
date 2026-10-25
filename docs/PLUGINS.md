# 插件使用与声明格式

顶部橙色入口统一管理启动器与 RH Code 的插件，内置插件可以按依赖启停；「社区插件」接收 JSON 声明或带摘要的 `.pclext.json` 包。社区插件可以提供输入框、笔记、开关、选择器和整数控件，并在授权后应用外观或复制笔记；也支持页面跳转和实例摘要；内置组件与原生服务见[产品插件开发](RH_CODE_PLUGINS.md)。商店暂未开放。

## 内置：实例备份与分享

在「实例设置 → 概览」中使用「实例备份与分享」：

1. 选择「实例备份」或「精简联机包」。备份包括已识别的模组、配置、资源包、光影、存档及服务器列表；联机包省略存档、服务器列表及已知位置的个人设置。
2. 点击「检查备份内容」，查看文件数量、大小和导出提示。
3. 点击「另存为 ZIP」选择位置；任务在现有任务管理中执行，可在提交前取消。

ZIP 使用 PCL RH 的本地导出格式。需要复制到一个新实例时，在「实例选择 → 导入整合包」选择此 ZIP，并指定新名称。它不是自动还原到原目录，也不是直接覆盖存档。完整导出选项仍在「实例设置 → 导出」。

这两个快捷预设不包含截图、日志、未知目录和公共运行库／资源库，不是整个文件夹的镜像；在其他设备导入后，可能需要补全对应平台的依赖。分享预设按已知文件位置排除个人内容，自定义模组配置仍应自行检查。

该插件 ID 为 `launcher.instances.snapshots`，可在橙色 Plugin 入口启停。停用撤回组件与未提交的界面操作，已接纳的导出任务继续由任务管理负责。它不启动 RH Code 或 AI，也不会自动扫描或备份游戏。

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
| `launcher` | `home.secondary`、`tools.cards`、`settings.cards` | 启动器首页、插件管理预览或个性化设置 |
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
| `navigate` | `launcher.navigate` | 固定主页面或设置子页，完整列表见插件规范 |
| `show-instance-summary` | `instances.summary` | 不接受目标路径；使用启动器当前选择 |
| `set-launcher-appearance` | `launcher.appearance` | 应用限定的主题、光标和右键菜单外观 |
| `set-launcher-downloads` | `launcher.downloads` | 应用 1～64 并发、0～1024 MiB/s 总限速 |
| `copy-text` | `clipboard.write` | 复制本插件保存的文字 |

所有声明都需要必需的 `ui.cards` 权限。实例摘要仅包含游戏版本、加载器、模组数量和隔离状态，不包含实例名、文件路径或账号。没有选中实例或缺少操作权限时，相应按钮不可用。

打包工具可通过 `experimental/extensions/src/package.mjs` 的 `createPackage(source)` 生成带 SHA-256 摘要的 JSON 包。摘要用于发现内容不匹配，不验证发布者身份。

## 可交互组件与示例

组件有明确的保存按钮；修改后先保存，再执行卡片操作。恢复默认只重置草稿，不会直接覆盖已保存值。插件自己的数据会跨重启保留，卸载时删除；不要在笔记中保存密钥。

可以直接导入以下 JSON：

- [外观切换](../experimental/extensions/examples/appearance-switcher.json)：在首页和个性化设置选择主题、鼠标样式，保存后点击「应用外观」即可真正改变启动器。需要可选的外观授权。
- [启动检查清单](../experimental/extensions/examples/launch-checklist.json)：保存备忘开关与目标帧率数字，附实例管理入口。清单不替你备份存档或修改游戏设置。
- [工作区笔记](../experimental/extensions/examples/workspace-notes.json)：在 RH Code 的「插件卡片」区域编辑笔记与阶段，授权后可复制已保存笔记。

停用或卸载外观插件后，已应用的主题保持原样；可在个性化设置中调整。完整的字段、权限、操作和生命周期契约见[插件规范 v1](PLUGIN_SPEC.md)。

## 在 RH Code 单独摆放插件

在任意区域顶部的「区域内容」中选择插件卡片，就能只显示该面板；名称显示卡片标题，标题与插件名称不同时会附上插件名称。可拆分成多个区域，也可与文件、图谱或 AI 对话并排。「插件卡片」仍可显示全部工作区卡片。

选择「插件操作」可搜索并执行已授权的操作；这些操作使用已保存设置。停用或卸载插件后，布局位置保留为不可用占位，之后重新启用相同面板会恢复。示例[工作区快捷操作](../experimental/extensions/examples/workspace-shortcuts.json)可以与工作区笔记同时导入，分别摆放。


## 扩展首页与设置子页

插件可以在首页提示区、快捷操作区添加组件，也可以在启动、Java、管理、个性化、语言和杂项设置中添加卡片。卡片可使用紧凑布局、语义提示、保存值和个人进度条；它们沿用启动器的主题与控件。

- [启动助手](../experimental/extensions/examples/launcher-dashboard.json)：在首页显示快捷设置入口，在启动设置记录自己的检查进度与备注。
- [下载预设](../experimental/extensions/examples/download-presets.json)：在设置 → 管理保存并发数和限速，再点击「应用下载配置」；首页快捷卡片也能应用已保存预设。需要授予下载配置权限，0 表示不限速。

保存插件预设和应用启动器配置是两个操作。有进行中的文件任务、游戏、更新或登录时，请结束后再切换下载策略。插件不会自动发起下载，停用后已应用配置保持原值。
