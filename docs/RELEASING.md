# 发行维护 / Releasing

版本与附件见 [GitHub Releases](https://github.com/HyacineFengJin/PCL-RH/releases)。本文说明维护者构建、验证和发布的流程；安装与源码构建见[构建与运行](BUILDING.md)。Linux x64 为主要支持平台，Windows x64、macOS Intel / Apple Silicon 为实验性。发行包未签名，macOS 未经 Apple 公证；具体功能和实机验收范围见[平台说明](PLATFORMS.md)。

## 构建发行文件

1. 确认待发行的 `master` 已提交并通过对应检查，应用版本与锁文件保持一致。记录待发行提交，不混用旧候选与新源码。
2. 在 **Actions → [Official native candidates](https://github.com/HyacineFengJin/PCL-RH/actions/workflows/official-native.yml) → Run workflow** 选择 `master` 和所需平台，通常为 `all`。流程固定使用 Cargo `release` 优化配置，并从仓库 Secret 读取官方 CurseForge 构建凭据；缺少凭据会失败。
3. 原生流程构建 WebUI、桌面和 CLI，按实际目标依赖生成 `native-attribution.json` 与 `NATIVE-DEPENDENCIES.txt`，验证实际归档、逐文件哈希、桌面 `--build-info`、CLI 版本及声明型插件宿主，并生成 Windows 当前用户安装器。保留每个平台的源提交、运行编号、清单及校验结果。
4. 用同一次已结束、且所需便携包已经通过验证的官方构建 `source_run` 运行下表中的独立分发流程。它们复用经核验的原始程序；程序源提交和打包工具提交分别记录。独立安装布局要求程序支持 `split-v1`。

| 工作流 | 产物与验证 |
| --- | --- |
| [Linux system-package verification](https://github.com/HyacineFengJin/PCL-RH/actions/workflows/linux-distribution.yml) | Ubuntu 24.04 x64 `.deb`；真实安装、升级、移除、重装和 purge，检查生产资源、插件宿主与用户数据保留 |
| [macOS app distribution](https://github.com/HyacineFengJin/PCL-RH/actions/workflows/macos-distribution.yml) | Intel / Apple Silicon 独立 `.app` 与应用 DMG；只读资源、独立数据、LaunchServices、自有原生窗口正常退出，以及 DMG 挂载和复制检查 |
| [Windows installer verification](https://github.com/HyacineFengJin/PCL-RH/actions/workflows/windows-installer.yml) | 安装工具单独变化时复用原官方 Windows 程序，检查安装、快捷方式、原生窗口正常关闭、占用失败、重装、升级与保留数据卸载 |

普通 **Desktop platform builds** 可生成不含官方服务凭据的预览；`release` 仅表示编译优化配置。这些构建工作流不创建 tag、不发布 GitHub Release，也不执行签名或公证。Actions artifact 保留 7 天，不能作为长期下载入口。

## 验证与发布

发布前下载本次产物并核对以下信息：

- 归档中的版本、提交、平台和架构与所选构建一致；桌面程序报告 `release`、生产 WebUI、所需资源布局和正确的内置服务声明，CLI 版本另行核对。
- 每个安装文件或归档的 SHA256 与构建输出一致；再打包产物中的源归档摘要能追溯至原始官方构建。版权与许可资料随包保留。
- 所选平台的打包和安装检查实际成功。旧运行的成功记录不代表新产物通过；编译、安装、原生窗口与游戏进入分别记录。

[Native client acceptance](https://github.com/HyacineFengJin/PCL-RH/actions/workflows/game-acceptance.yml) 可用同一 `source_run` 检查原版与 Fabric 客户端。通过条件包含持续可见窗口、渲染初始化、游戏写入的 Quick Play 记录、临时单人世界进入和所属进程清理。该流程不验证 Microsoft 登录、认证启动或人工游戏操作。软件渲染的通过结果也不能代替实体显卡兼容性验证。

在 GitHub 创建对应版本的 Release 草稿，将 tag 关联到本次发行的明确源提交，上传核验过的实际安装文件、便携归档和校验文件，避免只附 Actions 外层 artifact ZIP。各平台同名清单或归档应添加可区分的平台与架构名称，并同步校验文件中的文件名。发布前复核草稿中的文件清单、摘要和说明，再公开 Release。

发行说明至少包含：

- 本版功能、修复和优化，以及每项变更的适用范围。
- Linux 为主要支持平台，Windows/macOS 为实验性；包未签名，macOS 未经 Apple 公证。
- 本次实际构建和验收范围。实体 Windows/macOS 游戏验收未完成时明确保留该状态，不用原生窗口检查代替。
- Windows/macOS 私有安装的 Java 处理器、RH Code 源码写回与隔离构建、Windows 运行中 EXE 替换等现有限制，详见[平台说明](PLATFORMS.md)。
- [安装、更新及数据位置](BUILDING.md)；如涉及数据兼容性变化，写清迁移方式。

公开后检查 Release 附件能下载，重新核对摘要，并确认最新正式版入口指向本次发行。README 的日常下载入口使用 `/releases/latest`；说明某一版的能力或验收结果时使用明确版本链接，预发布版本也使用明确版本链接。
