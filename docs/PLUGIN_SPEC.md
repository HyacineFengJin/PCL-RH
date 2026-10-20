# PCL RH 插件规范 v1

本规范描述 PCL RH 的 0.9.0 插件模块提供的声明接口。清单的 `schemaVersion` 和宿主 API 仍为 `1`；新增字段、组件及能力采用增量扩展。旧宿主遇到不认识的字段或能力会拒绝导入。这里的 0.9.0 是插件模块版本，并非启动器的整体版本。

## 1. 两种扩展方式

- **社区声明插件**：通过 JSON 安装，由宿主创建组件、保存插件数据、检查授权并执行固定操作。本文定义其格式。
- **内置代码插件**：与应用一起编译，可以贡献工作区视图、命令、布局和 AI 工具。使用同一套组件生命周期；开发接口见[内置插件开发](RH_CODE_PLUGINS.md)。

社区声明不能载入 JavaScript、程序集、网页、Shell 或自行替换 Rust 服务。JSON 的 `id` 不会成为原生命令名。社区插件也不能借用内置模块的 ID 获取内置能力。

## 2. 清单与包

清单必须是 UTF-8 JSON。解析器拒绝重复对象键、保留键 `__proto__` / `constructor` / `prototype`、未知字段和不支持的格式。完整输入最多 64 KiB，嵌套深度最多 12；文字标签不能含控制字符或方向覆盖字符。

| 字段 | 要求 |
| --- | --- |
| `format` | 固定为 `pcl-linux.declarative-extension`；旧名称保留用于格式兼容 |
| `schemaVersion` | `1` |
| `id` | 2～8 段小写命名空间，例如 `local.my-plugin`；每段以字母开头，随后允许字母、数字、短横线；总长不超过 128 |
| `name`、`publisher` | 非空纯文字，分别最多 80 个字符；发布者名字不是身份认证 |
| `version` | 三段稳定版本号，例如 `1.2.0`；每段 0～999999，不允许前导零 |
| `api` | `{ "min": 1, "maxExclusive": 2 }`；宿主版本必须落在左闭右开的整数区间 |
| `scope` | 可选：`launcher`、`rh-code`、`both`；省略时为 `launcher`，不改写原清单或摘要 |
| `capabilities` | 唯一的能力声明列表，元素为 `{id, required, reason}`；原因最多 240 字符 |
| `settings` | 可选，最多 16 个设置定义，见下一节 |
| `contributions` | `{ "cards": [...] }`；1～12 张卡片 |

修改同一 ID 的清单必须提高版本号；相同版本不允许换内容。降级不通过普通导入。升级和重新授权都需要新的确认，不能自动取得新增权限。

`.pclext.json` 是 JSON 外包格式，不是 ZIP。它包含 `packageFormat: "pcl-linux.declarative-package"`、`packageVersion: 1`、`manifest` 以及 SHA-256 `integrity.manifestDigest`。整个包也必须在 64 KiB 内。可用 `experimental/extensions/src/package.mjs` 的 `createPackage(source)` 生成、`readPackage(source)` 验证。摘要检测内容变化，不证明发布者身份。

[JSON Schema](../experimental/extensions/schema/manifest.schema.json)提供编辑器结构提示；跨字段引用、授权关系和保存值的检查以运行时校验为准。

## 3. 组件与插件数据

设置定义的公共字段为 `{id, label, type, default}`。局部 ID 以小写字母开头，随后允许小写字母、数字、短横线，最长 48 个字符，插件内唯一。标签最多 100 字符。

| `type` | 额外字段 | 对应组件与值 |
| --- | --- | --- |
| `text` | 必填 `maxLength`：1～4000；可选 `multiline` 布尔值 | 输入框或多行笔记；字符串，允许空值和首尾空白 |
| `toggle` | 无 | 开关；布尔值 |
| `select` | `options`：2～16 个 `{value, label}` | 宿主选择器；默认值必须属于选项，选项值唯一且最多 64 字符 |
| `number` | `min`、`max`：有序整数，范围在 ±1000000 内 | 整数输入框；默认值与保存值必须在范围内 |

文字长度按 Unicode 码点计数。多行文本允许换行与制表符；仍拒绝 NUL、其他控制字符和方向覆盖字符。单行文本不允许换行或制表符。设置值不是凭据存储，请勿放 API Key 或账号令牌。

卡片结构：

```json
{
  "id": "notes",
  "slot": "rh-code.cards",
  "title": "工作区笔记",
  "text": "保存后可复制。",
  "fields": ["note"],
  "actions": [{"id": "copy", "label": "复制笔记", "kind": "copy-text", "setting": "note"}]
}
```

`fields` 可省略，最多引用 8 个不同设置 ID。宿主按引用顺序渲染组件；同一设置可以出现在多张卡片中，共用已保存值。卡片标题最多 100 字符，正文最多 2000 字符，每张卡片最多 4 个不同 ID 的操作，操作标签最多 64 字符。所有内容都按文字呈现，不接受 HTML、自定义 CSS 或动态表达式。

组件编辑只更新草稿。用户点击「保存」后，宿主验证类型与当前清单，再原子保存。失败保留原数据与当前草稿。「恢复默认」只改草稿，仍需保存。卡片有未保存修改时，操作按钮暂停，防止执行时误用旧的保存值。

插件数据属于插件 ID，跨启动器重启保留，不按 RH Code 项目分别存储。升级只保留新定义仍接受的同名值；删除的字段不再保留，不兼容字段使用新默认值。每个插槽的全部卡片投影合计最多 768 KiB，重复展示字段也计入预算；超限的安装、启用或保存会被拒绝，保留原状态。插件持久存储总计最多 3 MiB。没有迁移脚本。卸载删除声明、授权和插件数据；重新安装从默认值开始。

## 4. 展示位置与生命周期

| `slot` | 可用范围 | 位置 |
| --- | --- | --- |
| `home.secondary` | `launcher` / `both` | 启动器首页 |
| `tools.cards` | `launcher` / `both` | 社区插件页预览 |
| `settings.cards` | `launcher` / `both` | 设置 → 个性化 |
| `rh-code.cards` | `rh-code` / `both` | RH Code 的「插件卡片」区域，可拆分、移动或关闭 |

`scope` 决定可使用的展示位置，不是新权限。插件管理和卡片通过轻量宿主工作，不需要加载 AI 引擎。页面隐藏或被替换后，旧事件和迟到结果不能导航或修改新页面。插件声明不持有页面对象、原生接口或后台线程。

启停、撤权、卸载和安全模式会刷新已经挂载的卡片。停用保留授权与数据；安全模式暂停所有社区卡片且跨重启保留。撤销必需能力后插件暂停，点击启用不会自动恢复授权。卸载后不会继续展示组件。

## 5. 能力与操作

| 能力 | 允许的用途 |
| --- | --- |
| `ui.cards` | 创建卡片与受控组件；必须声明为必需 |
| `storage.settings` | 保存本插件声明的设置；存在设置定义时必须声明为必需 |
| `launcher.navigate` | 用户点击后进入固定启动器页面 |
| `instances.summary` | 用户点击后查看所选实例的受限摘要 |
| `launcher.appearance` | 用户点击后修改限定的启动器外观字段 |
| `clipboard.write` | 用户点击后复制本插件已保存的文本；不允许读剪贴板 |

每个操作必须声明对应能力。未授予可选能力时，只禁用相应操作；插件不能自行授予能力。保存、点击和原生执行都核对当前插件状态；渲染过的按钮不构成持续授权。

### 固定页面与实例摘要

- `navigate`：附带 `target`，仅支持 `launch`、`instances`、`downloads`、`tools`、`settings`。
- `show-instance-summary`：不附带路径或目标。只显示当前实例的游戏版本、加载器、模组数、隔离状态，不包含名称、路径或账号。

### 修改启动器外观

```json
{
  "id": "apply",
  "label": "应用外观",
  "kind": "set-launcher-appearance",
  "values": {"theme": {"setting": "theme"}, "custom_cursor": true}
}
```

`values` 只允许下表中的字段，至少一个。每个值可以是常量，或精确的 `{ "setting": "设置ID" }` 引用。引用不能计算表达式；布尔字段须引用开关，枚举字段须引用其全部选项都在允许范围内的选择器。

| 字段 | 允许值 |
| --- | --- |
| `theme` | `system`、`light`、`dark` |
| `custom_cursor` | 布尔值 |
| `cursor_style` | `accent`、`outline` |
| `cursor_size` | `small`、`standard` |
| `custom_context_menu` | 布尔值 |
| `context_menu_density` | `comfortable`、`compact` |

执行读取已保存的插件设置，经 Rust 再次验证后只更新这些外观字段，其他启动器设置保持原值。成功后立即刷新启动器外观。停用或卸载插件不会撤销已经应用的外观；可随时从个性化设置修改。

### 复制笔记

`copy-text` 必须用 `setting` 引用一个 `text` 定义。宿主只复制该项已保存的文字，不接受点击参数临时替换内容。系统不支持剪贴板或调度失败时显示错误，不伪报完成；没有读取剪贴板、自动粘贴或发送内容的权限。

## 6. 宿主接口与提交边界

此处接口供宿主组件、开发测试和内置适配器使用，社区声明本身不能直接调用。

- `extensions_review` / `extensions_confirm`：确认令牌绑定精确清单和所选能力，具有有效期且只能使用一次。
- `extensions_cards`：按插槽读取当前投影；只投影该卡片引用的设置定义及保存值。
- `extensions_settings`：参数 `{id, expected:{digest,generation}, values}`，部分更新 1～16 项。必须携带当前展示的快照，不接受未知键或错误类型。此操作的完整请求上限为 512 KiB，其他普通请求仍为 128000 字节。
- `extensions_action`：参数 `{extensionId, cardId, actionId, expected}`；从已安装清单寻找操作，不接受调用者提供原生命令或外观补丁。
- `extensions_enable` / `extensions_disable` / `extensions_revoke` / `extensions_remove`：管理已安装声明；界面携带当前快照以拒绝过期操作。

`digest` 标识清单内容，`generation` 标识当前宿主中的状态代次；不能把代次保存为跨重启的永久授权。数据提交后才发布新卡片及代次。保存失败不会先改变正在运行的值。更改设置、授权或启停状态都会使旧动作失效。

原生外观和复制操作在同一次宿主准入中验证授权并执行；已经准入并完成的作用不会因之后的撤权自动回滚。界面使用完成通知重新读取当前外观，避免过时快照覆盖较新的设置。

## 7. 示例与检查

三个可直接导入的例子：

- [外观切换](../experimental/extensions/examples/appearance-switcher.json)：首页与个性化设置中的选择器和开关，能实际应用启动器外观。
- [启动检查清单](../experimental/extensions/examples/launch-checklist.json)：持久开关与整数备忘，附实例管理入口。它不会替用户备份存档、修改帧率或阻止启动。
- [工作区笔记](../experimental/extensions/examples/workspace-notes.json)：多行文本、阶段选择器与原生复制。

开发时可运行：

```sh
node --test experimental/extensions/tests/settings.test.mjs
node --test experimental/extensions/tests/store-lifecycle.test.mjs experimental/extensions/tests/lightweight-host.test.mjs
node --test apps/desktop/scripts/extensions-settings-ui.test.mjs
```

这些检查覆盖清单、授权、保存与组件事件，不替代使用者对本机桌面集成的验证。安装操作见[插件使用说明](PLUGINS.md)。

## 8. RH Code 独立面板与操作目录

每张当前启用且已授权的 `rh-code.cards` 卡片，都可以在 RH Code 的「区域内容」选择器单独选择。显示名为卡片标题，标题与插件名称不同时会附上插件名称；同一面板可以在多个区域展示，沿用区域的拆分、交换、最大化和关闭操作。原有「插件卡片」聚合组件继续保留。

面板 ID 由宿主生成：`rh.extensions.panel.` 加上 `SHA256(JSON.stringify([extensionId, cardId]))` 的 64 位小写十六进制值。JSON 数组使用 UTF-8 字节。相同 ID 对跨设置变更、版本升级与重新安装保持相同面板 ID；改变卡片 ID 会产生新面板。布局只保存该标识及区域状态，不授予能力或执行代码。停用、必需权限撤销、安全模式或卸载后，面板不可用但布局占位保留；相同 ID 再次可用时恢复。

「插件操作」是另一个可选区域组件，支持按操作名、卡片标题和插件名称搜索。它列出当前 RH Code 卡片声明的操作，不包含仅属于启动器范围的操作。未授权或缺少所选实例时显示不可用原因。选择操作使用已保存的设置；搜索、加载目录和刷新本身不执行操作。

新增的宿主读取接口：

- `extensions_catalog` 返回 `{panels, actions}`，只包含当前 RH Code 卡片的身份、标题、摘要与状态代次及操作是否可用，不包含设置值、笔记正文、账号、项目文件或目标路径。
- `panels` 元素含 `viewId`、`extensionId`、`extensionName`、`cardId`、`title`、`digest`、`generation`。
- `actions` 元素含 `extensionId`、`extensionName`、`cardId`、`cardTitle`、`id`、`label`、`kind`、`enabled`、`unavailableReason`、`digest`、`generation`。目录不是可执行回调；仍须通过原有 `extensions_action` 核对当前声明和授权。
- `extensions_cards` 可附带 `filter: {extensionId, cardId}`，读取一个确切卡片；未知组合或不属于所求插槽的卡片返回空数组。省略过滤条件时沿用原有完整插槽读取。

完整目录和每个卡片插槽各自受 768 KiB 投影预算约束。发布后才出现新组件；超限安装或保存会保留原状态。可信内置 `rh.extensions` 模块负责把目录转换成宿主视图，社区声明不会获得 React 渲染函数、Cordis 上下文或原生 API。隐藏时撤回动态视图，模块休眠或停用时取消订阅；重新进入后读取当前状态。

[工作区快捷操作示例](../experimental/extensions/examples/workspace-shortcuts.json)演示一个 `both` 范围插件在首页提供入口、在 RH Code 提供独立面板与搜索操作。
