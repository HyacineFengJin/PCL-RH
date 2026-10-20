# 第三方代码与许可

主项目原创部分使用 [MIT](LICENSE)，范围见 [COPYRIGHT.md](COPYRIGHT.md)。下列第三方内容保留各自的版权与许可证，不能仅凭主项目的 MIT 声明将它们改为 MIT。

## 直接使用与直接纳入

| 组件 | 使用方式 | 许可与署名来源 |
| --- | --- | --- |
| React / React DOM | 前端依赖，实际版本见 npm 锁文件 | MIT；保留 Meta / Facebook 及贡献者的原声明 |
| Tauri / Tauri API | Rust 桌面宿主与前端 IPC | MIT OR Apache-2.0；各版本原文见汇总 |
| Lucide React | 受控界面图标 | ISC；使用包内实际声明，不归为 PCL 原创图标 |
| `@deepseek-ai/cordis` 4.0.4 | RH Code 插件生命周期 | MIT，`Copyright (c) 2021-present Shigma`；[独立原文](LICENSES/CORDIS.txt) |
| `@earendil-works/pi-coding-agent` 1.0.4 及 Pi 模块 | 模型调用和 Agent 适配 | MIT，原版权 `2025 Mario Zechner`；补文依据为对应版本的上游提交，并记录在清单中 |
| `experimental/runtime/vendor/maker/` | 直接纳入并适配 | MIT，Mod Creator Lab contributors；[原文](experimental/runtime/vendor/maker/LICENSE) |
| `experimental/runtime/vendor/porter/` | 直接纳入并适配 | MIT，Mod Port Lab contributors；[原文](experimental/runtime/vendor/porter/LICENSE) |
| 游戏 / 加载器图像 | 用于界面识别 | Minecraft 原权利及 EULA；Fabric 标志 CC0-1.0（unascribed）；NeoForged 网站图像 CC-BY-4.0。详见[素材声明](apps/desktop/src/assets/game-icons/NOTICE)及[固定来源](apps/desktop/src/assets/game-icons/SOURCES.json) |
| Noto Sans CJK SC 子集 | 随包字体 | OFL-1.1，Adobe；[许可](apps/desktop/src/assets/fonts/LICENSE)及[来源与修改说明](apps/desktop/src/assets/fonts/NOTICE) |

PCL / PCL CE、HMCL、PCL N / Nex 和 DSH 的参考关系另见版权说明。参考项目不是上述自写代码的统一版权人；同样，参考关系不赋予重新授权其源码或标志的权利。

## 可再生成的依赖清单

- [THIRDPARTY.generated.json](THIRDPARTY.generated.json)：精确版本、依赖边、使用范围、原许可证表达式、正文指纹及证据缺口。
- [LICENSES/DEPENDENCIES.txt](LICENSES/DEPENDENCIES.txt)：收集到的完整 LICENSE / COPYING / NOTICE 原文，按内容 SHA-256 去重，保留上游署名及逻辑来源路径。
- [生成工具](tools/licenses/README.md)：离线采集、内容一致性检查和严格模式说明。

当前快照采集 Linux `x86_64-unknown-linux-gnu` 的 Cargo 依赖闭包，以及桌面和 Pi runtime 的两份 npm 锁文件。构建/开发依赖也会列出，不能把“出现在锁文件中”直接等同于“编入了发行二进制”。其他平台的原生依赖、未安装的可选 npm 包及操作系统动态库不冒充已完成检查。

当前清单中 **2 个已安装包的许可正文证据尚未收齐**：`@napi-rs/lzma-linux-x64-gnu` 1.5.1、`proxy-agent-negotiate` 1.1.0。它们已有 MIT 许可声明，但精确版本中尚未找到可确认的完整授权原文；未以作者名或其他版本的正文代替。另有 **85 个未安装的可选 npm 包**，仅记录锁文件声明。这些项在 `issues` 中单独列明。严格证据检查因此不会报告全通过；补齐或更新依赖后应重新生成，而不是删除这些提示。

对归档缺正文但已核实上游的包，清单保留精确版本、固定提交、原文指纹及发布来源。Cargo 补文另核对发布归档校验和、源码提交和原始 manifest；npm 补文保留对应发布元数据。有分支许可时只采用已核实分支，例如 fastnbt 的 MIT；selectors 的 MPL 2.0 正文依据该版本源码头指定的官方版本链接。

## 包内嵌套许可与系统库

不能只看最外层包装包的 `license`：

- `libdbus-sys` 的 Rust 包装层声明 MIT / Apache-2.0，其 `vendor/dbus` 带有 AFL-2.1 / GPL-2.0-or-later 选择及个别工具的独立条款；对应 COPYING 文本已收集。具体编译和分发内容须按其自身条款处理。
- `openssl` / `openssl-sys` 及 vendored `openssl-src` 的内部声明、Apache / OpenSSL 相关许可分别保留。
- `ring` 的 ISC、Apache 与包内 BoringSSL、fiat、once_cell 等声明分别保留；不能统一标成 MIT。
- Linux 图中还包含 MPL-2.0 的 cssparser、cssparser-macros、dtoa-short、option-ext、selectors；相关文件及修改须按对应许可处理。
- Unicode-3.0、`unicode-ident` 的组合许可、webpki-roots 的 CDLA-Permissive-2.0，以及 npm 中的 BSD、ISC、BlueOak、Unlicense 等原声明均保留。
- GTK / GDK / GLib、WebKitGTK / JavaScriptCore、libsoup 等由操作系统提供的库另有 LGPL、BSD 等许可。源码依赖清单不代替系统包声明，也不证明某个具体二进制已经满足重打包、静态链接或替换库方面的要求。

## 分发

随源码或应用分发时保留主许可证、版权范围、第三方说明、许可正文汇总和第三方目录内的原有许可证。若分发新的平台包、修改第三方库或改变链接方式，应按实际构建再次核对对应源代码和许可要求。

应用的「设置 → 关于 → 许可与版权声明」可离线查看这些资料，大份正文按需加载。独立分发附件可由 `python3 tools/licenses/release.py` 生成，包含固定资料及逐文件指纹；该附件和阅读器不将上述证据缺口视作已解决。

清单记录可追溯证据与缺口，既不授权权利人未授予的权利，也不是“所有代码皆原创”“全部依赖均为 MIT”或“所有发行方式均已合规”的保证。
