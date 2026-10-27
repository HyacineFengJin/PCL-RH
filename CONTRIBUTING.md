# 贡献指南

感谢参与 PCL RH。使用问题从[使用指南](docs/USAGE.md)排查，代码导航见[开发文档](docs/DEVELOPMENT.md)，完整入口见[文档目录](docs/README.md)。

## 开始开发

当前完整桌面开发环境为 Linux。依赖与首次构建步骤见[构建指南](docs/BUILDING.md)，其他平台的实际支持范围见[平台说明](docs/PLATFORMS.md)。

只预览 WebUI：

```sh
npm ci --prefix apps/desktop
npm run preview:data --prefix apps/desktop
npm run dev --prefix apps/desktop
```

浏览器预览使用示例数据；启动游戏、文件操作等功能由桌面 Rust 宿主提供。进行 Tauri 开发时，在 `apps/desktop/` 中执行 `npm run tauri -- dev`。

AI 运行时使用独立锁文件，修改相关代码时安装其依赖：

```sh
npm ci --prefix experimental/runtime --ignore-scripts
```

## 组织修改

- 一次修改聚焦一个问题，说明用户会看到什么变化。
- 可独立启停的能力优先通过现有插件入口、贡献槽位和类型化服务实现，沿用统一授权与生命周期。参见[内置插件开发](docs/RH_CODE_PLUGINS.md)。
- Rust 负责原生权限和实际作用，WebUI 负责交互。保留取消、中断恢复、旧格式兼容及不覆盖用户修改的语义。
- 注释解释所有权、约束与设计原因。使用者说明写入对应文档，README 保持项目概览。
- 引入第三方代码或素材时保留原许可、来源与修改说明；新增依赖同步锁文件和版权资料。

## 检查修改

按改动范围运行相关回归，避免为了文档或纯静态内容修改反复构建整个桌面应用。

```sh
# WebUI 类型检查与生产构建
npm run build --prefix apps/desktop

# 便携核心、认证和网络
cargo test --locked -p pcl-core -p pcl-auth -p pcl-network

# 声明插件存储和轻量宿主
node --test experimental/extensions/tests/store-lifecycle.test.mjs experimental/extensions/tests/lightweight-host.test.mjs

# 许可输入与随附文件
python3 tools/licenses/release.py --check

# 补丁格式
git diff --check
```

原生桌面回归可用 `cargo test --workspace --locked --features pcl-desktop/custom-protocol`。完整测试可能需要更多内存及系统依赖；CI 检查入口位于 `.github/workflows/`。请在 PR 中如实注明已执行的检查和未验证的平台。

## 提交与反馈

Bug 反馈请提供应用版本、系统、复现步骤及相关错误。附日志前移除访问令牌、API Key、邮箱、服务器地址和其他不希望公开的信息；不要上传账号配置或完整私人游戏目录。

仓库保留产品源码、可复现的自动测试、合成夹具与必要文档。游戏包、个人 Mod 试验、真实账号数据、历史截图、构建产物和私人开发记录留在本地，提交前检查 `git diff --cached`。

代码和文档贡献须有权以项目 [MIT License](LICENSE) 提供；第三方内容继续遵守其原许可，不能用项目 MIT 覆盖。具体范围见[版权与来源](COPYRIGHT.md)。
