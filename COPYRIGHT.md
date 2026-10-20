# 版权与来源

## 主项目许可

PCL RH 维护者以 [MIT License](LICENSE) 授权本项目的原创代码、文档和可授权的原创素材。版权署名为 `Copyright (c) 2026 PCL RH contributors`。使用、修改或分发这些内容时须保留 MIT 的版权声明及许可文本；MIT 允许商业使用和闭源复用。

第三方代码、字体、系统库和外部内容不因放入同一仓库或发行包而改为 MIT。各自的版权、许可和随附声明优先适用于对应内容，见下面的边界及[第三方说明](THIRD_PARTY_NOTICES.md)。MIT 不代替商标授权，也不表示本项目获得 Minecraft 或参考项目的官方认可。

## 代码来源分类

| 范围 | 来源及处理 |
| --- | --- |
| `crates/`、`apps/desktop/src/`、`apps/desktop/src-tauri/src/` | PCL RH 的 Rust / React / TypeScript 实现；第三方素材和依赖另列，不包含在“全部原创代码”的笼统声明中 |
| `code/`、`experimental/extensions/src/`、`experimental/runtime/src/` 及宿主适配器 | 本项目的插件、Agent 和工作流适配代码；使用 Cordis / Pi 的部分按依赖边界处理 |
| `tools/game-title-agent/` 和 `crates/core/assets/game-title-agent.jar` | 本项目 Java 源码及其构建产物；生成方式见该工具说明，不是 LWJGL 或 Minecraft 的二进制副本 |
| `experimental/extensions/compat/n-probe/` | 本项目编写的兼容性探针；通过外部项目引用使用被测 SDK。SDK 本身未并入本仓库，另受其对应版本许可约束 |
| `experimental/runtime/vendor/maker/` | 引入并适配的 Mod Creator Lab 代码，保留原 MIT 文本及 `2026 Mod Creator Lab contributors` 署名 |
| `experimental/runtime/vendor/porter/` | 引入并适配的 Mod Port Lab 代码，保留原 MIT 文本及 `2026 Mod Port Lab contributors` 署名 |
| npm / Cargo 依赖 | 不属于 PCL RH 原创代码；版本由锁文件固定，许可和原作者声明见生成的依赖清单与正文汇总 |

`vendor/` 中的本项目修改不删除原作者署名。`experimental/SOURCES.json` 记录纳入的模块版本与文件指纹；它是来源记录，不是新的许可证。

## 设计参考与实际引入

- **PCL / PCL CE**：启动器家族的界面和交互设计参考。PCL RH 的界面以 React / CSS 实现，不把参考项目的自定义许可改为 MIT，也不向使用者授予其名称或标识的使用权。
- **HMCL**：认证流程、启动器功能和格式兼容的参考。此前纳入的部分图像已从当前源码树替换，不再以一份 HMCL GPL 文本推定它能授权所有游戏贴图或项目标志。
- **PCL N / Nex**：平台分层、插件声明及兼容性研究的参考；兼容报告和探针不表示原项目插件代码获得 MIT 再许可。
- **DeepSeek Harness**：插件生命周期、Agent 组织和上下文机制的参考。当前直接使用的是 **Cordis** 包，不是完整 DSH Agent 的整体移植；Cordis 的原作者为 Shigma，保留其 [MIT 文本](LICENSES/CORDIS.txt)。
- **Pi SDK**：模型调用与 Agent 执行适配使用固定版本 SDK，保留其原有 MIT 许可与上游版权，不归到 DeepSeek 或 PCL RH 名下。

设计参考、协议实现、接口兼容和直接复制代码是不同关系。后续直接纳入其他项目源码时，必须保留其具体许可、版权及修改记录，不能用本项目 MIT 覆盖。

## 素材

| 内容 | 当前许可边界 |
| --- | --- |
| 项目标志及 `assets/branding/` | 主要美术使用 GPT 制作，维护者选用并整合；在本项目能够授予的权利范围内随 MIT 提供。AI 工具署名不是第三方素材授权证明 |
| `apps/desktop/src/assets/cursors/` | 为本项目绘制的 SVG，随本项目 MIT 提供 |
| `apps/desktop/src/assets/game-icons/` | 当前为本项目绘制的通用 SVG 与原创示例皮肤，随 MIT 提供；不包含此前借用的游戏贴图、Steve 皮肤或加载器标志，详见目录 NOTICE |
| `apps/desktop/src/assets/fonts/` | Noto Sans CJK SC 子集，版权及 **SIL Open Font License 1.1** 独立保留；不改为 MIT |
| 致谢中的作者和项目 | 保留文字署名与链接，使用项目生成的字母占位头像，不再随包包含外部头像及网站 favicon |
| 用户导入/下载的游戏、模组、皮肤、披风、整合包、截图和插件 | 属于各自权利人，未因此获得本项目 MIT 授权 |

## 版本和分发范围

本说明适用于当前源码树，不追溯重授旧版本中已经删除的第三方图片。Git 历史中保留的旧素材仍受原权利约束。

分发时应同时保留 `LICENSE`、`NOTICE`、本说明、`THIRD_PARTY_NOTICES.md`、依赖许可正文以及各第三方目录的独立许可证。对依赖的修改、静态引入、重打包和系统库组合还须遵守其原许可。依赖清单列明采集范围与缺口；它不是对所有平台、所有构建产物的法律合规保证。
