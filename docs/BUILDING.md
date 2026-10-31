# 构建与运行 / Build and run

桌面构建已通过 Linux、Windows 和 macOS 原生 CI；功能与发行包的适配范围见[平台说明](PLATFORMS.md)。

## 依赖

- Rust / Cargo。
- Node.js 22.19+ 与 npm。
- Python 3.11+。
- C / C++ 构建工具，以及 GTK 3、WebKitGTK 4.1、OpenSSL、D-Bus 的开发依赖。
- 安装桌面快捷入口时需要提供 `desktop-file-validate` 的软件包。

Linux 中执行 RH Code 的隔离构建还需要 bubblewrap 0.8+ 和可用 JDK；普通游戏启动不预先加载该构建环境。

系统依赖通过所用发行版的软件包管理器安装。文件选择器需要 XDG Desktop Portal 和适合桌面环境的后端；记住正版登录需要可用的系统凭据服务。运行游戏所需 Java 可以在启动器中下载。

Linux builds require Rust / Cargo, Node.js 22.19+ / npm, Python 3.11+, C / C++ build tools, and development dependencies for GTK 3, WebKitGTK 4.1, OpenSSL and D-Bus. A desktop portal provides file dialogs; a system credential service supports remembered accounts. Java runtimes can be downloaded from the launcher.

## 构建

```sh
git clone https://github.com/HyacineFengJin/PCL-RH.git
cd PCL-RH
./build-native.sh
./start-native.sh
```

构建脚本按锁文件安装 WebUI 与 AI 运行时依赖，生成前端并构建 Rust 桌面与 CLI。默认同时使用两个编译任务，内存较小时可调整：

```sh
PCL_BUILD_JOBS=1 ./build-native.sh
```

应用入口为 `./start-native.sh`。安装桌面快捷入口：

```sh
./install-desktop.sh
```

首次使用请添加已有 `.minecraft` 或空游戏目录，再选择已有实例、下载游戏或导入整合包。详细操作见[使用指南](USAGE.md)。

The build script installs locked WebUI and AI runtime dependencies, builds the frontend and native executables, and prepares the desktop entry point. Set `PCL_BUILD_JOBS=1` to reduce compiler concurrency. Run `./install-desktop.sh` to install a desktop shortcut once `desktop-file-validate` is available.

## 跨平台便携构建（适配中）

已有原生工具链的 Windows、macOS 和 Linux 可以使用同一个构建入口：

```sh
python tools/build-native.py --jobs 2
```

脚本构建当前主机的桌面与 CLI，将结果放入 `.pcl-rust/bin`；`--build-only` 只保留 `target` 中的产物。Windows 用 `start-native.ps1` 启动，Linux/macOS 用 `start-native.sh`。这仍是可写项目文件夹中的便携布局，不是已完成的 Windows 安装器或 macOS `.app` 发行包。当前通过情况与未完成能力以[平台说明](PLATFORMS.md)为准。

## 开发与检查

WebUI 预览、Tauri 开发、相关测试和贡献方式见[贡献指南](../CONTRIBUTING.md)，实现导航见[开发文档](DEVELOPMENT.md)。
