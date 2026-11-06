# 构建与运行 / Build and run

发行文件见 [v0.2.0](https://github.com/HyacineFengJin/PCL-RH/releases/tag/v0.2.0)，也可按下文[从源码构建](#源码构建)。**Linux x64 为主要支持平台；Windows x64、macOS Intel / Apple Silicon 为实验性支持。** 发行包未签名，macOS 未经 Apple 公证；功能限制与验收范围见[平台说明](PLATFORMS.md)。

## 安装与更新

以下说明适用于对应类型的发行附件，以实际提供的文件为准。安装后添加已有 `.minecraft` 或空游戏目录，再选择实例、下载游戏或导入整合包。详细操作见[使用指南](USAGE.md)。下载时可用同目录的 `.sha256` 文件核对完整性。

### Linux

便携版：解压 `.tar.gz`，保持整个 `PCL-RH` 目录完整，在其中运行 `./start-native.sh`。可在「设置 → 百宝箱 → 创建快捷方式」创建应用入口；系统需要 `desktop-file-validate`。更新前退出启动器及仍在使用它的游戏，保留用户数据并替换程序文件。

Ubuntu / Debian 包：`.deb` 面向 Ubuntu 24.04 x64 ABI，其他 Debian 系发行版须满足包中声明的库与版本要求。下载后在文件所在目录安装：

```sh
sudo apt install ./pcl-rh_0.2.0-1_amd64.deb
pcl-rh
```

将示例文件名替换为实际下载的名称。菜单入口与 `/usr/bin/pcl-rh` 使用同一包装入口，程序及只读资源位于 `/usr/lib/pcl-rh`。升级前退出启动器，再用包管理器安装更高版本；`apt remove pcl-rh` 或 `apt purge pcl-rh` 保留用户数据、存档和工程。当前没有 RPM 或 Arch 系统包。

### Windows（实验性）

安装版：运行 `*-windows-x86_64-release-setup.exe`，默认安装到当前用户的本地应用目录，并创建开始菜单入口。需要 [Microsoft WebView2 Runtime](https://developer.microsoft.com/en-us/microsoft-edge/webview2/)。

便携版：解压 `*-windows-x86_64-release.zip`，保留完整资源目录，双击 `pcl-desktop.exe`。

升级前退出启动器及仍在使用它的游戏，再在原目录重新安装或手动换包。安装器只替换程序清单中的文件，卸载保留运行后产生的实例、设置和工程。发生文件占用等错误时，处理原因后重新运行安装器。升级没有完整事务回滚，也不会清理新版清单中已移除的旧资源；需要清理旧程序资源时，可先卸载再安装。运行中 EXE 的自动替换尚未实现。

Windows Java 路径需能由当前系统代码页无损表示；无法表示时，只接受已经存在且核对为同一对象的兼容短名。没有可用短名会明确拒绝，需换到可表示的 Java 或游戏目录；启动器不修改系统 8.3 设置。

### macOS（实验性）

按 CPU 选择 Apple Silicon（`aarch64`）或 Intel（`x86_64`）文件。打开 `*-app.dmg`，把 `PCL RH.app` 拖到 Applications 或复制到个人应用目录，再打开副本；也可从应用归档中解压完整 `.app`。应用和 DMG 均未签名、未经 Apple 公证；最低系统版本以对应构建说明为准，既有分发检查的宿主为 macOS 15。

更新前退出启动器并替换完整 `.app`。程序与 CLI 位于 `Contents/MacOS`，生产资源位于 `Contents/Resources/PCL-RH`，用户数据在应用包外保留。

若使用便携 `.tar.gz`，须把整个 `PCL-RH` 目录放到可写位置，再打开 `Start PCL RH.command` 或运行 `./start-native.sh`；它与独立 `.app` 的数据布局不同。

### 用户数据与运行依赖

独立安装版的默认用户数据目录为：

| 平台 | 数据目录 |
| --- | --- |
| Linux `.deb` | `$XDG_DATA_HOME/pcl-rh`；未设置时为 `~/.local/share/pcl-rh` |
| macOS `.app` | `~/Library/Application Support/PCL RH` |

两种安装版均可通过 `PCL_RH_DATA_ROOT` 指定另一个绝对数据目录，该目录须位于程序与发行资源之外。安装版不会自动迁移旧便携版的设置、账号记录或保存凭据；原数据仍在原目录，已有 Minecraft 目录可重新添加。

- Linux 需要可用的 GTK 3、WebKitGTK 4.1、OpenSSL 和 D-Bus 运行库；文件选择器需要 XDG Desktop Portal 及适合桌面环境的后端，记住正版登录需要当前平台可用的系统凭据服务。
- 游戏 Java 可由启动器下载，发行包不附完整 JDK。
- RH Code 和社区声明型插件需要单独安装 Node.js 22.19+。内置 npm 依赖随包提供；没有社区插件记录时，首页和设置中的空插槽不会为读取空内容启动 Node。
- 插件宿主使用系统标准位置的 Node：Linux 为 `/usr/bin/node`，Windows 为 Program Files 下的 `nodejs/node.exe`，macOS 为 `/opt/homebrew/bin/node` 或 `/usr/local/bin/node` 及其受支持的 Homebrew 安装。NVM 等用户级目录目前不会自动识别。安装或升级后可点击界面中的「重试」。

## 依赖

从源码构建需要：

- Rust / Cargo。
- Node.js 22.19+ 与 npm。
- Python 3.11+。
- Linux：C / C++ 构建工具，以及 GTK 3、WebKitGTK 4.1、OpenSSL、D-Bus 的开发依赖；安装桌面快捷入口另需 `desktop-file-validate`。
- Windows：Visual Studio 的 C++ 桌面开发工具、MSVC Rust 工具链和 WebView2。
- macOS：Xcode Command Line Tools 和当前主机架构的 Rust 工具链。

各系统依赖的安装方法见 [Tauri 官方前置依赖说明](https://v2.tauri.app/start/prerequisites/)。Linux 中执行 RH Code 的隔离构建还需要 bubblewrap 0.8+、支持 pidfd 的 Linux 内核和可用 JDK；普通游戏启动不预先加载该构建环境。

## 源码构建

Linux：

```sh
git clone https://github.com/HyacineFengJin/PCL-RH.git
cd PCL-RH
./build-native.sh
./start-native.sh
```

构建脚本按锁文件安装 WebUI 与 AI 运行时依赖，生成前端并构建桌面与 CLI。默认使用两个编译任务，内存较小时可运行 `PCL_BUILD_JOBS=1 ./build-native.sh`。安装桌面快捷入口可运行 `./install-desktop.sh`。

已有原生工具链的 Windows、macOS 和 Linux 也可使用：

```sh
python tools/build-native.py --jobs 2
```

Windows 结果放在项目根目录，可打开 `pcl-desktop.exe` 或使用 `start-native.ps1`；Linux/macOS 放入 `.pcl-rust/bin`，用 `start-native.sh` 启动。`--build-only` 只保留 `target` 中的产物。该命令准备可写的便携目录；安装器、`.app` / DMG 与 `.deb` 由独立分发工作流生成。

维护者可在 **Actions → Desktop platform builds → Run workflow** 选择 `debug` 或 `release` 和目标平台生成预览。普通预览不含官方 CurseForge 服务凭据；Actions artifact 保留 7 天，下载后须再解压其中的归档。官方构建、安装包生成与发布过程见[发行维护](RELEASING.md)。

## 开发与检查

WebUI 预览、Tauri 开发、测试和贡献方式见[贡献指南](../CONTRIBUTING.md)，实现导航见[开发文档](DEVELOPMENT.md)。历史平台检查与实体游戏验收的限制见[平台说明](PLATFORMS.md)。
