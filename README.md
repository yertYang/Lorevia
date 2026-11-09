# Lorevia

[![最新正式版](https://img.shields.io/github/v/release/yertYang/Lorevia?label=最新正式版)](https://github.com/yertYang/Lorevia/releases/latest)

## 下载安装包

**[查看最新正式版及全部安装包](https://github.com/yertYang/Lorevia/releases/latest)** · [历史版本与发布说明](https://github.com/yertYang/Lorevia/releases)

<!-- desktop-downloads:start -->
当前最新正式版为 **[v1.7.3](https://github.com/yertYang/Lorevia/releases/tag/v1.7.3)**，发布于 2026 年 11 月 5 日。请选择适合自己设备的安装包：

| 操作系统 | 适用设备 | 下载 v1.7.3 |
| --- | --- | --- |
| macOS | Apple Silicon（M 系列芯片） | [下载 DMG](https://github.com/yertYang/Lorevia/releases/download/v1.7.3/Lorevia-MacOS-Apple-Silicon-v1.7.3.dmg) |
| macOS | Intel 芯片 | [下载 DMG](https://github.com/yertYang/Lorevia/releases/download/v1.7.3/Lorevia-MacOS-Intel-v1.7.3.dmg) |
| Windows | x64 安装版 | [下载 Setup.exe](https://github.com/yertYang/Lorevia/releases/download/v1.7.3/Lorevia-Windows-v1.7.3-Setup.exe) |
| Windows | x64 便携版 | [下载 Portable.zip](https://github.com/yertYang/Lorevia/releases/download/v1.7.3/Lorevia-Windows-v1.7.3-Portable.zip) |
<!-- desktop-downloads:end -->

### 安装说明

- **macOS**：打开 DMG，将 Lorevia 拖入“应用程序”。
- **Windows 安装版**：运行 Setup.exe，按向导完成安装。
- **Windows 便携版**：解压 Portable.zip 后启动应用。

当前版本尚未配置 macOS Apple Developer ID 签名与公证、Windows 代码签名，首次打开可能出现系统安全提示。安装包信息及校验值请查看对应版本的 [发布说明](https://github.com/yertYang/Lorevia/releases)。

支持 GitHub 更新的新版客户端可在 **设置 → 关于 → GitHub 桌面端更新** 中下载已签名的核心更新包，安装前会备份数据；桌面客户端本身使用最新安装包升级。旧客户端需先手动安装包含此功能的新版。

## 开发与部署

- [Docker 开发环境](docs-for-dev/docker-development.md)
- [开发环境内存配置](docs-for-dev/development-memory.md)
- [服务器部署与更新](deployment/README.md)
- [桌面端自动发布与 GitHub 更新](docs-for-dev/desktop-releases.md)
