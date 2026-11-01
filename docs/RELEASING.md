# 发行候选与首发

发行目标为 Linux x64、Windows x64、macOS Intel 与 Apple Silicon。[四平台优化便携包](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41272954394)、[Windows 当前用户安装器](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41297103941)、[两架构 macOS 独立 `.app` / 应用 DMG](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274750933)及 [Ubuntu 24.04 x64 `.deb`](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274751702)均已通过实际分发验收。候选未签名、macOS 未经 Apple 公证，完整跨平台账号和游戏验收仍未完成。

## 生成候选

在官方仓库的 **Actions → Official native candidates → Run workflow** 选择 `master`，平台默认 `all`。流程复用普通桌面构建，固定 Cargo `release` 配置，并从仓库 Secret 取得官方 CF 构建凭据；没有该 Secret 时失败，不生成声称支持官方 CF 的包。

每个平台会得到版本化归档、SHA256、文件清单与版权资料。打包仅选 HEAD 中的生产资源，不包含私人工作目录、账号、密钥文件、测试游戏或预建用户数据。解压后检查逐文件哈希、桌面编译信息、CLI 版本与声明型插件宿主 RPC。Windows 另生成当前用户安装器，并在临时 runner 验证实际安装、快捷方式、原生窗口和保留数据的卸载；不登录账号或启动游戏。普通 **Desktop platform builds** 仍可生成无官方 CF 凭据的预览。

只修改安装工具或验收脚本时，可在 **Windows installer verification** 输入原始官方运行的 `source_run`，复用原始 Windows 程序，无需重新编译 Rust。流程核对生产仓库、分支、提交、唯一对应 artifact 与下载摘要；报告分别记录程序源提交与安装工具提交。

## 独立安装包与客户端验收

以下三条手动流程同样接受 `source_run`，须选择含对应平台有效便携产物的 **Official native candidates** 运行。新安装布局要求程序支持 `split-v1`，旧便携程序会被编译信息检查拒绝。再打包会复用原始程序，保持源提交与工具提交分开记录。

| 工作流 | 产物与检查范围 |
| --- | --- |
| [macOS app distribution](https://github.com/HyacineFengJin/PCL-RH/actions/workflows/macos-distribution.yml) | Intel / Apple Silicon 的 `macos-app-*` 和 `macos-app-dmg-*`；逐文件校验、只读应用资源、独立数据目录、真实插件宿主及退役、LaunchServices 诊断入口、自有原生窗口，以及 DMG 只读挂载、复制与卸载 |
| [Linux system-package verification](https://github.com/HyacineFengJin/PCL-RH/actions/workflows/linux-distribution.yml) | `linux-debian-package`；以 Ubuntu 24.04 x64 ABI 生成 `.deb` 和运行库依赖，在临时 runner 实际安装、升级、移除、重装和 purge，核对程序文件并保留用户数据 |
| [Native client acceptance](https://github.com/HyacineFengJin/PCL-RH/actions/workflows/game-acceptance.yml) | `native-client-evidence-*`；四平台实际安装布局中的 Minecraft 1.21.4 原版及 Fabric 0.20.1 客户端、官方 Java、可见窗口、渲染初始化、Quick Play 临时单人世界进入与所属进程清理 |

当前已通过验收的安装候选可从以下 artifact 下载；先解压 Actions artifact，再打开其中的安装包或应用归档：

| 平台 | 安装候选 |
| --- | --- |
| Windows x64 | [windows-user-installer-release](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41297103941/artifacts/13079827391)，内含 `pcl-rh-0.2.0-windows-x86_64-release-setup.exe`；[portable-preview-windows-2025-release](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41297103941/artifacts/13079782410)，内含完整便携 ZIP |
| macOS Apple Silicon | [macos-app-macos-15](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274750933/artifacts/13071236999)，内含 `mac-app.tar.gz`；[macos-app-dmg-macos-15](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274750933/artifacts/13070867662)，内含 `pcl-rh-0.2.0-macos-aarch64-release-app.dmg` |
| macOS Intel | [macos-app-macos-15-intel](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274750933/artifacts/13070923080)，内含 `mac-app.tar.gz`；[macos-app-dmg-macos-15-intel](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274750933/artifacts/13070668626)，内含 `pcl-rh-0.2.0-macos-x86_64-release-app.dmg` |
| Ubuntu 24.04 x64 | [linux-debian-package](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274751702/artifacts/13071036014)，内含 `pcl-rh_0.2.0-1_amd64.deb` |

macOS 两种架构的验收均包含真实启动器原生窗口的连续可见与正常退出、只读应用资源、独立用户数据、LaunchServices，以及从只读 DMG 直接运行诊断和复制后复验。Debian 包已实际通过安装、升级、移除、重装和 purge，验证默认及显式数据目录、真实插件宿主退役及用户数据保留。

应用与系统包的用户数据独立于程序：macOS 默认 `~/Library/Application Support/PCL RH`；Linux 默认 `$XDG_DATA_HOME/pcl-rh`，未设置该变量时为 `~/.local/share/pcl-rh`。`.app` 更新时替换完整应用，`.deb` 通过包管理器安装更高包版本，均保留外部数据；安装方法和便携版切换说明见[构建与运行](BUILDING.md)。

真实客户端流程只有原版和 Fabric 均取得窗口、渲染与世界进入证据，并清理所属进程后才通过；失败也保存日志和阶段结果。[Linux 两类客户端已通过完整流程](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41275607901)，使用 Xvfb / Mesa 软件渲染。macOS 的 [Apple Silicon](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41278654369)与 [Intel](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41278656406)检查均报告 `GLFW 65545: NSGL: Failed to find a suitable pixel format`，未创建游戏窗口或进入世界，实体 Mac 游戏验收仍待完成。Windows 的[原版和 Fabric 检查](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41299432729)已完成官方安装、Java 获取、加载器启动及所属进程清理，但 CI 驱动报告 GLFW 65542 / WGL 不支持 OpenGL，未取得渲染或世界进入证据，实体 Windows 验收仍待完成。当前没有四平台完整通过结果。该流程不进行 Microsoft 登录、认证启动或人工游戏操作，也不能代表所有显卡、加载器、整合包或游戏版本已通过验证。

这些流程在临时 runner 中执行安装和测试，产物保留 7 天；不会替换使用者的本机安装，不创建 tag、发布 Release、签名或 Apple 公证。`release` 是编译优化配置，不代表正式发行。

## 发行范围与验证状态

| 范围 | 当前情况 |
| --- | --- |
| 四平台编译、便携归档与基础自动化检查 | 已有流程及通过记录，见[平台说明](PLATFORMS.md) |
| 优化便携候选 | 最新四平台运行均成功，已核对实际归档、二进制信息、CLI 与声明型宿主 |
| Windows 安装与原生窗口 | [最新官方候选](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41297103941)已通过生产编译、便携包检查、实际安装、Unicode 快捷方式、窗口正常关闭、占用失败、重装、升级及保留数据卸载 |
| macOS 独立应用与 Linux 系统包 | 两架构 `.app` / 应用 DMG 与 Ubuntu `.deb` 均通过实际验收，下载与范围见上文 |
| 真实原版 / Fabric 离线客户端 | Linux 原版与 Fabric 完整流程通过；两种 macOS CI 均因 GLFW 65545 / NSGL 像素格式错误而失败，实体 Mac 待验收；Windows 已到加载器并清理所属进程，CI 因 GLFW 65542 / WGL 驱动不支持 OpenGL 而失败，实体 Windows 待验收 |
| 正版登录、认证启动、人工操作与整合包实测 | 完整跨环境验收待完成；启动器窗口或离线世界进入检查不能代替 |
| 私有整合包的 Java 处理器 | Windows/macOS 尚缺受限执行后端，实际含客户端处理器的计划提前报告不支持 |
| RH Code 跨平台能力 | Windows/macOS 源码写回、作者代码受限构建与部分工具生命周期仍需完成 |
| Windows 自更新 | 已有下载与校验，运行中 EXE 的替换 helper 待完成；便携候选采用手动换包 |
| 签名与公证 | 候选未签名，macOS 未经 Apple 公证 |

发布说明以对应候选的实际验收结果为准，编译、安装、原生窗口和游戏进入是不同范围的证据。用户操作与数据兼容见[使用指南](USAGE.md)、[RH Code 指南](RH_CODE.md)和[平台说明](PLATFORMS.md)。

最近的[核心检查](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41298267684)、[Code 边界检查](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41298267651)和[安装事务检查](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41298267661)均在四个原生平台通过。
