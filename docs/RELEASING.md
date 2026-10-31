# 发行候选与首发

首发目标为 Linux x64、Windows x64、macOS Intel 与 Apple Silicon。[四平台优化候选包](https://github.com/HyacineFengJin/PCL-RH/actions/runs/41202128364)已在同一提交通过原生构建、解压与无界面运行检查，标准安装包与实机验证仍在完善。

## 生成候选

在官方仓库的 **Actions → Official native candidates → Run workflow** 选择 `master`，平台默认 `all`。流程复用普通桌面构建，固定 Cargo `release` 配置，并从仓库 Secret 取得官方 CF 构建凭据；没有该 Secret 时失败，不生成声称支持官方 CF 的包。

每个平台会得到版本化归档、SHA256、文件清单与版权资料。打包仅选 HEAD 中的生产资源，不包含私人工作目录、账号、密钥文件、测试游戏或预建用户数据。解压后检查逐文件哈希、桌面编译信息、CLI 版本与声明型插件宿主 RPC；桌面检查不打开窗口或启动游戏。普通 **Desktop platform builds** 仍可生成无官方 CF 凭据的预览。

这些流程只准备候选，不创建 tag、发布 Release、签名或替换本机安装。`release` 构建配置与正式公开发行是两个步骤。

## 首发还差什么

| 工作 | 当前情况 |
| --- | --- |
| 四平台编译、便携归档与基础自动化检查 | 已有流程及通过记录，见[平台说明](PLATFORMS.md) |
| 优化配置的完整候选验证 | 四个平台已通过同一提交的候选构建与实际二进制信息核对；Actions artifact 保留 7 天 |
| 干净系统的窗口、登录、Java、原版/Fabric、整合包和游戏测试 | 仍需在真实 Windows、macOS 与 Linux 完成，CI 无界面检查不能替代 |
| 私有整合包的 Java 处理器 | Windows/macOS 尚缺受限执行后端，实际含客户端处理器的计划提前报告不支持 |
| RH Code 跨平台能力 | Windows/macOS 源码写回、作者代码受限构建与部分工具生命周期仍需完成 |
| Windows 自更新 | 已有下载与校验，运行中 EXE 的替换 helper 待完成；便携候选采用手动换包 |
| 安装与分发体验 | 系统运行依赖、标准安装包、签名与发布说明仍需收尾 |

首个公开测试版可以明确标出能力范围并收集实际反馈；三平台功能一致的正式版本还需要完成对应移植。发版前须按实际通过的能力撰写说明，不能把编译通过等同于所有加载器、AI 服务和游戏都已验证。用户操作与数据兼容以[使用指南](USAGE.md)、[RH Code 指南](RH_CODE.md)和[平台说明](PLATFORMS.md)为准。
