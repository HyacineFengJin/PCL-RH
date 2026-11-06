<p align="center">
  <img src="assets/pcl-rh.png" alt="PCL RH 图标" width="112" />
</p>

# PCL RH

**启动 Minecraft，管理你的游戏，创作自己的模组。**

PCL RH 是使用 **Rust + Tauri + React** 构建的 Minecraft: Java Edition 启动器，延续 PCL 家族的使用习惯，并通过 RH Code 与插件体系扩展创作能力。

[下载最新版](https://github.com/HyacineFengJin/PCL-RH/releases/latest) · [English](README.en.md) · [使用指南](docs/USAGE.md) · [文档](docs/README.md)

## 主要功能

- **游戏启动与管理**：安装 Minecraft 和模组加载器，导入整合包，管理多个隔离实例、模组与资源。
- **下载与维护**：并行下载、Java 自动匹配、依赖补全、备份恢复，以及游戏日志和崩溃预分析。
- **RH Code**：面向模组项目的 AI 工作区，支持多个对话、源码编辑、改动审阅、项目构建和像素资产工具。
- **插件体系**：启动器与 RH Code 共用插件管理，支持可选官方功能和经授权的社区组件扩展。

## 下载与使用

从 [GitHub Releases](https://github.com/HyacineFengJin/PCL-RH/releases/latest) 下载对应系统的安装包：

| 系统 | 推荐文件 | 支持状态 |
| --- | --- | --- |
| Linux x64 | Ubuntu 24.04 `.deb` 或便携 `.tar.gz` | 主要支持 |
| Windows x64 | `*-setup.exe` 或便携 `.zip` | 实验性 |
| macOS Apple Silicon / Intel | 对应架构的 `*-app.dmg` | 实验性 |

Windows/macOS 的实机游戏验收尚未完成，部分功能仍有限制，详见[平台支持](docs/PLATFORMS.md)。发行包未签名，macOS 未经 Apple 公证。

[安装与更新](docs/BUILDING.md) · [游戏与整合包](docs/USAGE.md) · [RH Code](docs/RH_CODE.md) · [插件](docs/PLUGINS.md) · [问题反馈](https://github.com/HyacineFengJin/PCL-RH/issues/new/choose)

从源码构建见[构建说明](docs/BUILDING.md#源码构建)；代码结构与参与方式见[贡献指南](CONTRIBUTING.md)。

## 许可与致谢

原创代码与文档采用 **[MIT License](LICENSE)**。第三方内容保留各自许可，见[版权与来源](COPYRIGHT.md)。

特别感谢 **[龙腾猫跃（LTCatt）](https://github.com/LTCatt)** 创作 [Plain Craft Launcher（PCL / PCL2）](https://github.com/Meloong-Git/PCL)，以及 [PCL CE](https://github.com/PCL-Community/PCL-CE)、HMCL 等项目的启发。可通过[官方赞助入口](https://meloong.com/afd/a/LTCat)支持龙猫；PCL 的[原许可与使用指南](LICENSES/PCL-LICENCE.txt)独立保留。

PCL RH 是第三方参考 PCL / PCL CE 设计、以 Rust 与 WebUI 独立实现的项目，非龙腾猫跃或 PCL CE 团队的官方发行，也非 Minecraft 官方产品。项目图标及主要美术由 **GPT** 制作。
