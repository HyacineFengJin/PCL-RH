# 平台支持 / Platform support

PCL RH 的完整桌面应用目前只提供 Linux 实现。Windows、macOS 的适配已开始；核心能够解析其 Minecraft 元数据，但这不代表已经可以在这些系统运行完整启动器。

| 部分 | Linux | Windows | macOS |
| --- | --- | --- | --- |
| Minecraft 平台、架构规则与原生库分类 | 已实现 | 已实现 | 已实现，区分 Intel / Apple Silicon |
| Java 路径与官方运行时目录名 | 已实现 | 已实现 `.exe` 路径 | 已实现 JDK / `.bundle` 路径 |
| 有限时 Java 探测 | 已实现 | 尚未实现，返回明确错误 | 已实现 Unix 探测 |
| 账号密钥存储后端 | Secret Service | Windows Credential Store | macOS Keychain |
| 声明型插件的清单、授权与持久状态（独立 Node 模块） | 已实现 | 数据层可运行 | 使用相同 Node 接口，待验证 |
| 桌面宿主、安装与恢复事务、进程监控、发行包 | Linux 实现 | 尚未适配完成 | 尚未适配完成 |

架构判断不会把 `x86` 当成 `x86_64`，也不会把 ARM 当成 x64。原生库提取使用平台对应的 `.so`、`.dll`、`.dylib` / `.jnilib`；旧元数据中的 `${arch}` 按目标位数展开。Mojang 没有提供的 Java 平台会报告不可用，不会自动下载其他架构的运行时。仅有分类支持不表示所有 Minecraft 版本都有对应架构的原生库。

账号存储使用 [keyring 的平台后端](https://docs.rs/keyring/3.6.3/keyring/#credential-store-features)，账号标识保持兼容。Microsoft 正版服务接入状态仍见[账号说明](AUTHENTICATION.md)。

## 核心开发与检查

以下命令只构建和检查独立的核心、网络与认证模块，不构建桌面宿主：

```sh
cargo test --locked -p pcl-core -p pcl-network -p pcl-auth
```

`.github/workflows/portable-core.yml` 在 Linux、Windows、macOS 的原生 runner 上执行该命令。显式平台测试在每个系统检查不同架构的规则与分类；Unix 的进程、权限和符号链接测试仅在 Unix 执行，Windows 另有路径组件检查。测试采用本地夹具与回环服务，不读取真实账号、密钥环或游戏目录。

完整桌面构建目前仍需要 Linux 的 GTK / WebKitGTK 和文件事务接口。其他平台尚无可供日常使用的安装包；当前 `build-native.sh`、`start-native.sh` 与 `install-desktop.sh` 是 Linux 脚本。

## English

The complete PCL RH desktop application currently targets Linux. Cross-platform work has started in the portable core: Minecraft OS/architecture rules, native library classifiers and formats, Java layouts, runtime index keys, and platform-specific credential backends cover Windows and macOS as well.

The portable-core workflow tests the core, network and authentication crates on native Linux, Windows and macOS runners. These checks do **not** build the desktop application. Bounded Java probing is still unavailable on Windows; desktop integration, file transactions, process monitoring and distribution packages are not yet adapted for Windows or macOS. Architecture-specific Minecraft natives must also exist for the selected game version. The current build and desktop installation scripts remain Linux-specific.

## 插件模块

社区插件的声明校验、授权、启停、卸载与持久状态是独立 Node 模块，不依赖 GTK、WebKit、AI SDK 或 Linux 文件事务。轻量 stdio 入口也可在没有 AI 运行时的目录中工作；完整 AI 宿主只在需要时加载。

在项目根目录运行：

```sh
node --test experimental/extensions/tests/store-lifecycle.test.mjs experimental/extensions/tests/lightweight-host.test.mjs
```

`.github/workflows/plugins.yml` 为 Linux、Windows、macOS 配置独立插件检查。该检查覆盖插件数据层与协议，不构建桌面窗口、安装器或游戏进程。插件格式见[插件指南](PLUGINS.md)。
