# 构建与运行 / Build and run

当前完整桌面构建支持 Linux，Windows / macOS 的适配范围见[平台说明](PLATFORMS.md)。

## 依赖

- Rust / Cargo。
- Node.js 22.19+ 与 npm。
- Python 3.11+。
- C / C++ 构建工具，以及 GTK 3、WebKitGTK 4.1、OpenSSL、D-Bus 的开发依赖。
- 安装桌面快捷入口时需要提供 `desktop-file-validate` 的软件包。

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

## 开发与检查

WebUI 预览、Tauri 开发、相关测试和贡献方式见[贡献指南](../CONTRIBUTING.md)，实现导航见[开发文档](DEVELOPMENT.md)。
