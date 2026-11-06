# 平台支持

[下载最新版本](https://github.com/HyacineFengJin/PCL-RH/releases/latest) · [安装与更新](BUILDING.md) · [文档目录](README.md)

Linux x64 为主要支持平台，Windows x64 和两种 macOS 架构为**实验性支持**。以下说明以 v0.2.0 为基线；各版本的变化与验收范围见对应 Release。

## 选择发行包

| 系统 | 架构 | 发行文件 | 支持状态 |
| --- | --- | --- | --- |
| Linux | x64 | Ubuntu 24.04 `.deb`、便携 `.tar.gz` | 主要支持 |
| Windows | x64 | 当前用户安装器 `.exe`、便携 `.zip` | 实验性 |
| macOS | Apple Silicon（aarch64） | `.app` 压缩包、应用 DMG、便携 `.tar.gz` | 实验性 |
| macOS | Intel（x86_64） | `.app` 压缩包、应用 DMG、便携 `.tar.gz` | 实验性 |

Linux 发行构建以 Ubuntu 24.04 x64 为基线，其他发行版须满足[运行依赖](BUILDING.md#用户数据与运行依赖)。macOS 应用声明的最低系统为 11.0，当前分发检查使用 macOS 15。发行包未签名，macOS 未经 Apple 公证。

目前不提供 Linux ARM、Windows ARM 或 32 位发行包。能识别某个架构不代表已有该架构的启动器或所有游戏版本的原生库。

## 功能与限制

| 功能 | Linux | Windows / macOS |
| --- | --- | --- |
| 原版 / Fabric 安装、依赖补全与修复 | 已实现 | 原生安装检查通过，实机游戏仍待验收 |
| Java 下载、模组与资源管理、日志诊断 | 已实现 | 已接入原生端口，具体游戏与环境仍需验证 |
| 整合包导入与实例导出 | 已实现 | 已接入，完整业务仍待实机验证 |
| 需要私有重建游戏核心的整合包 | Java 处理器依赖 Landlock ABI 3 / openat2 | 受限 Java 处理器尚未实现，相关计划会明确阻止 |
| 社区声明型插件 | 支持，需系统 Node.js | 数据层与宿主检查通过，需系统 Node.js |
| RH Code 项目扫描与对话宿主 | 支持 | 已接入，完整 AI 使用仍待实测 |
| RH Code 源码写回与资源保存 | 支持 | 原生写入事务尚未实现，操作前明确拒绝 |
| RH Code 作者代码构建 | 使用 bubblewrap 隔离 | 隔离执行尚未实现，返回 blocked |

普通「下载游戏」中的加载器安装，与上表的私有核心重建不是同一入口。私有处理器限制不影响原版、Fabric、不含客户端处理器的计划，或无需重建核心的完整游戏包；也不代表所有 Forge / NeoForge 版本均已通过实机验证。

Windows 的 Java 和游戏路径需能由当前系统代码页无损表示；否则只接受已经存在、且核对为同一对象的兼容短名。无法满足时应换用可表示的路径，启动器不会更改系统 8.3 设置。

账号凭据分别使用 Linux Secret Service、Windows Credential Store 和 macOS Keychain。登录、凭据保存与故障处理见[账号说明](AUTHENTICATION.md)。完整跨环境的正版登录与认证启动验收仍待完成。

## 更新与数据

安装版按原分发方式更新：Linux 使用包管理器，macOS 替换完整 `.app`，Windows 退出启动器后重新运行安装器。便携版须完整换包并保留个人数据；Windows 运行中 EXE 的自动替换尚未实现。

Windows 首次启动后立即关闭曾出现超时，独立复验通过，原因仍待实机排查。安装包校验、更新方法与数据目录见[安装与构建](BUILDING.md)。

## 当前验证范围

- 四个平台的程序构建、便携包校验和对应安装检查均已完成。安装器、原生窗口和游戏启动分别验证。
- Linux 原版与 Fabric 在软件渲染环境中完成了临时单人世界进入检查；这不替代实体显卡和不同桌面环境的兼容性验证。
- Windows / macOS 的云端游戏测试分别受 WGL / NSGL 图形能力限制，未完成渲染与进入世界验证，仍需实体设备验收。
- Microsoft 登录、认证启动和人工游戏操作不属于上述离线客户端检查的通过范围。

每次发行的来源和结果保存在 Release 附带的 `release-manifest.json` 与原始构建清单中。开发者可继续阅读[原生接口与检查](development/PLATFORM_PORTS.md)或[发行维护](RELEASING.md)。
