<p align="center">
  <img src="assets/pcl-rh.png" alt="PCL RH 图标" width="112" />
</p>

# PCL RH

**启动 Minecraft，管理你的游戏，创作自己的模组。**

PCL RH 是使用 **Rust + Tauri + React** 构建的 Minecraft: Java Edition 启动器，延续 PCL 家族的使用习惯，并通过 RH Code 与插件体系扩展创作能力。

[English](README.en.md) · [使用指南](docs/USAGE.md) · [文档](docs/README.md)

## 主要功能

- **游戏启动与管理**：安装 Minecraft 和模组加载器，导入整合包，管理多个隔离实例、模组与资源。
- **下载与维护**：并行下载、Java 自动匹配、依赖补全、备份恢复，以及游戏日志和崩溃预分析。
- **RH Code**：面向模组项目的 AI 工作区，支持多个对话、源码编辑、改动审阅、项目构建和像素资产工具。
- **插件体系**：启动器与 RH Code 共用插件管理，支持可选官方功能和经授权的社区组件扩展。

目前完整桌面应用支持 **Linux**；Windows / macOS 正在适配。项目持续开发中，具体支持范围见[平台说明](docs/PLATFORMS.md)。

## 开始使用

目前以源码构建为主，先准备[构建依赖](docs/BUILDING.md)，然后运行：

```sh
git clone https://github.com/HyacineFengJin/PCL-RH.git
cd PCL-RH
./build-native.sh
./start-native.sh
```

[游戏与整合包](docs/USAGE.md) · [RH Code](docs/RH_CODE.md) · [插件](docs/PLUGINS.md) · [参与开发](CONTRIBUTING.md)

## 许可与致谢

原创代码与文档采用 **[MIT License](LICENSE)**。第三方内容保留各自许可，见[版权与来源](COPYRIGHT.md)。

项目图标及主要美术由 **GPT** 制作。感谢 PCL 家族、HMCL 等项目的启发与参考。PCL RH 独立开发，非 PCL CE 或 Minecraft 官方发行。
