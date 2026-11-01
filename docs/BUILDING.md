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

Linux 中执行 RH Code 的隔离构建还需要 bubblewrap 0.8+、支持 pidfd 的 Linux 内核和可用 JDK；普通游戏启动不预先加载该构建环境。

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

脚本构建当前主机的桌面与 CLI：Windows 结果放在项目根目录，Linux/macOS 放入 `.pcl-rust/bin`；`--build-only` 只保留 `target` 中的产物。Windows 可直接打开 `pcl-desktop.exe`，Linux/macOS 用 `start-native.sh`。此命令准备可写的便携目录；Windows 安装器、macOS `.app` / DMG 与 Linux `.deb` 由下述独立工作流生成。当前通过情况以[平台说明](PLATFORMS.md)为准。

## 便携测试包

维护者可在 **Actions → Desktop platform builds → Run workflow** 选择 `debug` 或 `release` 构建配置；平台默认 `all`，也可单独验证某个平台。各原生 runner 构建桌面与 CLI，生成对应的 `portable-preview-*` artifact；只有解压、文件校验和命令行 / 声明型插件宿主检查通过才上传测试包。`release` 是编译优化配置，不代表已经正式发行或签名。

下载对应架构的 artifact 后，继续解压其中的 ZIP（Windows）或 tar.gz（Linux/macOS），保持整个 `PCL-RH` 文件夹完整：

- Windows：双击 `pcl-desktop.exe`。
- macOS：在 Finder 打开 `Start PCL RH.command`，或在终端运行 `./start-native.sh`。
- Linux：运行 `./start-native.sh`，可在「设置 → 百宝箱 → 创建快捷方式」创建应用入口。

程序与资源位于包根目录，用户数据在首次运行时创建；更新时手动换包并保留原用户数据。Mac 的 `.command` 是便携启动入口，与下述独立 `.app` 使用不同的数据布局。

便携包包含 Code 与社区声明型插件服务的 npm 依赖，运行这些服务需要单独安装 Node.js 22.19+。首次启动且没有社区插件记录文件时，首页和设置中的社区插槽由 Rust 返回空内容，不为读取空内容启动 Node；插件管理与 RH Code 在实际使用时检查运行依赖。已有插件记录损坏或目录不可读会明确报告原因。系统 WebView / WebKit 依赖同样需要可用；普通测试包不包含官方 CurseForge 服务凭据。签名、标准安装包和实机游戏验证的状态见[平台说明](PLATFORMS.md)。

插件宿主使用系统标准位置的 Node：Linux 为 `/usr/bin/node`，Windows 为系统 Program Files 下的 `nodejs/node.exe`，macOS 为 `/opt/homebrew/bin/node` 或 `/usr/local/bin/node` 及其受支持的 Homebrew 安装。NVM 等用户级目录目前不会自动识别。安装或升级后可直接点击界面中的「重试」，无需重新启动整个启动器；权限、目录替换和存储损坏错误仍需按原原因处理。

维护者使用 **Official native candidates** 可从官方 `master` 构建包含官方 CF 服务凭据的四平台优化候选包，仍只上传 Actions artifact。[当前四平台优化候选](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41272954394)已全部成功。归档名包含应用版本；解压检查会实际读取桌面程序的 `--build-info` 并核对独立 CLI 版本，避免错装旧二进制或开发配置。候选构建与首发剩余工作见[发行说明](RELEASING.md)。

## Windows 安装候选

Windows 构建另提供 `windows-user-installer-release` artifact（普通调试构建对应 `debug`）。[最新独立安装验收](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41280219951)已通过实际安装、原生窗口正常关闭、占用失败后的重装、升级与保留数据卸载，可下载其中的 [windows-user-installer-retest](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41280219951/artifacts/13072798295)。解压后运行 `pcl-rh-0.2.0-windows-x86_64-release-setup.exe`，默认安装到当前用户的本地应用目录，并创建开始菜单入口。需要已安装 [Microsoft WebView2 Runtime](https://developer.microsoft.com/en-us/microsoft-edge/webview2/)。安装器与便携包均未签名。

升级前退出启动器及仍在使用它的游戏，再沿用原目录重新安装。安装器只替换程序清单中的文件，卸载保留运行后产生的实例、设置和工程。发生文件占用等错误时，处理原因后重新运行安装器；程序升级尚不提供完整事务回滚，也不会在升级时清理新版清单中已移除的旧资源。需要清除旧程序资源时，可先卸载再安装，保留的数据仍在原目录。

## macOS 独立应用候选

[macOS 独立应用与 DMG 验收](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274750933)已在 Apple Silicon 和 Intel 两种原生主机通过。检查包含只读应用资源、独立用户数据、真实插件宿主调用与退役、LaunchServices 入口，以及启动器自有原生窗口连续可见并正常退出。DMG 还通过只读挂载后直接运行应用诊断、复制应用、保留数据与卸载映像的检查。

| 架构 | 应用 DMG artifact | 应用归档 artifact |
| --- | --- | --- |
| Apple Silicon | [macos-app-dmg-macos-15](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274750933/artifacts/13070867662) | [macos-app-macos-15](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274750933/artifacts/13071236999) |
| Intel | [macos-app-dmg-macos-15-intel](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274750933/artifacts/13070668626) | [macos-app-macos-15-intel](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274750933/artifacts/13070923080) |

下载后先解压对应架构的 Actions artifact，再打开其中的 `.dmg`，把 `PCL RH.app` 拖到 Applications 或复制到个人应用目录，再打开副本；也可从 `mac-app.tar.gz` 解压完整 `.app`。程序与 CLI 位于应用的 `Contents/MacOS`，生产资源位于 `Contents/Resources/PCL-RH`，运行数据默认保存在 `~/Library/Application Support/PCL RH`，不写入应用包。移动或更新应用时，这个数据位置保持不变。

更新前退出启动器并替换完整 `.app`，保留外部用户数据和游戏目录。独立应用与 DMG 均未签名、未经 Apple 公证；最低系统声明按实际原生二进制的部署目标生成，两个架构的验收宿主为 macOS 15。

### 旧版便携 DMG

[旧便携映像验证](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41267794638)通过的是完整文件夹布局，artifact 名为 `macos-portable-dmg-macos-15` 和 `macos-portable-dmg-macos-15-intel`。使用这些旧包时，仍须将整个 `PCL-RH` 文件夹复制到可写位置，再打开其中的 `Start PCL RH.command`，并保留原目录中的 `.pcl-rust`、`Minecraft` 和自建工程。

## Linux Debian 包候选

[Linux 系统包验收](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274751702)已通过真实安装、升级、移除、重装和 purge，验证生产资源、默认及显式数据目录、真实插件宿主调用与退役，并确认保留用户数据。下载 [linux-debian-package](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41274751702/artifacts/13071036014) artifact，其中的包名为 `pcl-rh_0.2.0-1_amd64.deb`。目标为 Ubuntu 24.04 x64 ABI；系统库依赖由 `dpkg-shlibdeps` 根据实际 ELF 和构建系统的库信息生成，其他 Debian 系发行版须满足包中声明的库与版本要求。当前没有 RPM 或 Arch 系统包。

解压 artifact 后，用包管理器安装；后续候选请将命令中的文件名替换为下载的包名：

```sh
sudo apt install ./pcl-rh_0.2.0-1_amd64.deb
pcl-rh
```

应用菜单和 `/usr/bin/pcl-rh` 使用同一入口，由包装脚本定位 `/usr/lib/pcl-rh` 的程序与只读生产资源。用户数据默认在 `$XDG_DATA_HOME/pcl-rh`；未设置 `XDG_DATA_HOME` 时为 `~/.local/share/pcl-rh`。升级时先退出启动器，再用同一安装命令安装更高包版本。`apt remove pcl-rh` 或 `apt purge pcl-rh` 只移除包管理的程序文件，保留用户数据、存档和工程；软件包更新由包管理器完成。

macOS `.app` 和 Linux 安装版都可用 `PCL_RH_DATA_ROOT` 选择另一个绝对数据目录，该目录须位于程序与发行资源之外。它们不会自动迁移旧便携版的设置、账号记录或已保存凭据；原数据仍在原目录，已有 Minecraft 目录可在新安装版中重新添加。Node.js、游戏 Java、桌面门户和凭据服务的要求仍适用，包不会附带完整 Node.js 或 JDK。

## 实际游戏验收

[Linux 原版与 Fabric 客户端](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41275607901)已通过官方 Java 与游戏安装、可见窗口、临时单人世界进入及所属进程清理。该检查使用 Xvfb / Mesa 软件渲染，不能替代实际显卡、Microsoft 登录或人工游戏操作验证。

macOS 的 [Apple Silicon](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41278654369)和 [Intel](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41278656406)客户端检查均在 OpenGL 上下文创建时报告 `GLFW 65545: NSGL: Failed to find a suitable pixel format`，未创建游戏窗口或进入世界。Windows 更新后的程序候选与真实客户端仍待验收；启动器安装和窗口检查通过不表示这些游戏检查已通过。

## 开发与检查

WebUI 预览、Tauri 开发、相关测试和贡献方式见[贡献指南](../CONTRIBUTING.md)，实现导航见[开发文档](DEVELOPMENT.md)。
