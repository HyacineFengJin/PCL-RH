# 构建与运行 / Build and run

桌面构建已通过 Linux、Windows 和 macOS 原生 CI；功能与发行包的适配范围见[平台说明](PLATFORMS.md)。

## 依赖

- Rust / Cargo。
- Node.js 22.19+ 与 npm。
- Python 3.11+。
- Linux：C / C++ 构建工具，以及 GTK 3、WebKitGTK 4.1、OpenSSL、D-Bus 的开发依赖；安装桌面快捷入口另需 `desktop-file-validate`。
- Windows：Visual Studio 的 C++ 桌面开发工具、MSVC Rust 工具链和 WebView2。
- macOS：Xcode Command Line Tools 和当前主机架构的 Rust 工具链。

各系统依赖的安装方法见 [Tauri 官方前置依赖说明](https://v2.tauri.app/start/prerequisites/)。

Linux 中执行 RH Code 的隔离构建还需要 bubblewrap 0.8+ 和可用 JDK；普通游戏启动不预先加载该构建环境。

Linux 系统依赖通过所用发行版的软件包管理器安装，文件选择器需要 XDG Desktop Portal 和适合桌面环境的后端。记住正版登录需要当前平台可用的系统凭据服务。运行游戏所需 Java 可以在启动器中下载。

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

脚本构建当前主机的桌面与 CLI：Windows 结果放在项目根目录，Linux/macOS 放入 `.pcl-rust/bin`；`--build-only` 只保留 `target` 中的产物。Windows 用 `start-native.ps1` 启动，Linux/macOS 用 `start-native.sh`。这仍是可写项目文件夹中的便携布局，不是 Windows 安装器或 macOS `.app` 发行包。当前通过情况与未完成能力以[平台说明](PLATFORMS.md)为准。

## 便携测试包

维护者可在 **Actions → Desktop platform builds → Run workflow** 选择 `debug` 或 `release` 构建配置；平台默认 `all`，也可单独验证某个平台。各原生 runner 构建桌面与 CLI，生成对应的 `portable-preview-*` artifact；只有解压、文件校验和命令行 / 声明型插件宿主检查通过才上传测试包。`release` 是编译优化配置，不代表已经正式发行或签名。

下载对应架构的 artifact 后，继续解压其中的 ZIP（Windows）或 tar.gz（Linux/macOS），保持整个 `PCL-RH` 文件夹完整。Windows 运行 `start-native.ps1`，Linux/macOS 在终端运行 `./start-native.sh`。程序与资源位于包根目录，用户数据在首次运行时创建；更新时手动换包并保留原用户数据。

便携包包含 Code 的 npm 依赖，Node.js 仍需单独安装。系统 WebView / WebKit 依赖同样需要可用；测试包不包含官方 CurseForge 服务凭据。签名、标准安装包和实机游戏验证的状态见[平台说明](PLATFORMS.md)。

## 开发与检查

WebUI 预览、Tauri 开发、相关测试和贡献方式见[贡献指南](../CONTRIBUTING.md)，实现导航见[开发文档](DEVELOPMENT.md)。
