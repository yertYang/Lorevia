# 桌面端自动发布与 GitHub 更新

## 自动发布

推送代码到 `master` 后，`Desktop Release` 工作流会自动：

1. 根据根目录 `VERSION` 和已有正式版标签确定版本。未手动提高版本时自动递增补丁号，例如 `1.7.3` → `1.7.4`；手动提高到 `1.8.0` 则使用 `1.8.0`。
2. 将版本准备提交和 `vX.Y.Z` 标签一起推送。推送只允许快进，遇到并发提交不会覆盖分支；已经被新提交取代的旧任务会跳过。
3. 构建 macOS Apple Silicon、macOS Intel、Windows x64，生成四种安装包、三个已签名的核心清单和对应核心归档。
4. 三个平台全部成功并通过签名、SHA-256 校验后，上传全部附件并公开 GitHub Release。失败时不会公开一个缺少平台安装包的新版本；可在 Actions 中重新运行原工作流，沿用已准备的版本和标签。
5. 发布成功后更新 README 的版本号与下载表，再调用 Docker 镜像工作流发布同一标签的 GHCR 镜像。

`dev` 推送和默认的手动运行只验证构建。Pull Request 使用临时签名密钥且不发布。自动生成的版本和文档提交使用 `GITHUB_TOKEN`，不会再次触发发布循环。后续本地推送前需先拉取机器人生成的提交。

GitHub Actions 需要 `contents: write` 权限；`master` 的分支保护也必须允许发布机器人提交版本和 README。继续使用仓库已有的 `DESKTOP_RUNTIME_SIGNING_KEY`，它必须与 `apps/desktop/assets/runtime-public.pem` 匹配。Apple 签名和公证仍为可选配置。

自动公开 Release 使用 `GITHUB_TOKEN`，不会触发另一个 `release.published` 工作流，所以桌面流程会显式调用可复用的 `Docker Release`。人工发布 Release 仍保留原来的 Docker 构建触发方式。

## 从 GitHub 更新核心

在新版桌面客户端中打开 **设置 → 关于 → GitHub 桌面端更新**，或菜单 **核心 → 检查更新**：

1. 查询 `https://api.github.com/repos/yertYang/Lorevia/releases/latest`，仅考虑正式发布且比当前核心版本更高的版本。
2. 选择当前系统架构的 `Lorevia-Core-<target>.json`，校验客户端内置公钥对应的 Ed25519 签名、平台和版本。
3. 从同一个 Release 下载签名清单指定的核心归档，校验文件大小和 SHA-256。不完整、被篡改或来自其他仓库的附件会被拒绝，失败下载会清理。
4. 用户确认后停止核心，创建备份并安装新核心。新核心启动失败时沿用现有事务恢复原核心和数据。

下载过程不停止正在运行的核心。桌面壳本身（Electron、菜单、更新器等）的升级仍使用 Release 中的 DMG、Setup.exe 或 Portable.zip；核心包只升级内置服务与 Web 界面。历史版本若没有核心更新附件，会提供 GitHub 安装包入口。

已经发布的旧客户端没有 GitHub 更新入口，需要先下载安装包含本功能的新版客户端。Docker 服务器的页面内更新仍使用现有源码构建流程，详见 [服务器部署说明](../deployment/README.md)。

## 验证

遵循项目约定，在开发容器内执行：

```sh
docker compose -f compose.dev.yaml exec dev node --test scripts/desktop-release.test.mjs scripts/desktop-github-updates.test.mjs
docker compose -f compose.dev.yaml exec -w /workspace/web dev npx tsc --noEmit
```

本地测试覆盖版本选择、README 下载链接、Git 远端的原子版本提交、重复运行、过期提交跳过，以及三个平台的 GitHub 下载与签名校验。实际 macOS/Windows 安装包由 GitHub 托管的对应平台构建。

## 错误日志与自动清理

桌面端持久化日志以错误为主，文件为 `logs/errors-YYYY-MM-DD.jsonl`。正常版本检查、下载进度和成功事件只更新内存中的排错上下文，不逐条写入错误日志。出现错误时会记录 UTC 时间、操作编号 `operation_id`、失败阶段 `stage`、当前/目标版本、平台和架构、耗时、HTTP 状态、下载字节数、校验值、错误代码及堆栈。备份和回滚使用同一个操作编号，应用重启后的恢复流程也会沿用该编号。

在 **核心 → 查看运行日志** 中打开日志目录：

- macOS：`~/Library/Application Support/Open DeepTutor/logs/`
- Windows：`%LOCALAPPDATA%/Open DeepTutor/logs/`

核心服务的原有输出另存为 `runtime-YYYY-MM-DD.log`。错误日志单文件上限 5 MB，运行日志单文件上限 10 MB，各保留两份大小轮转归档。启动时及运行期间每小时清理一次，最多保留 14 天；已知日志总量超过 64 MB 时优先清理旧运行日志归档。清理也兼容旧版的 `runtime.log`、`runtime.log.previous`，不会跟随符号链接或处理业务数据、备份和其他文件。日志写入失败不会阻断更新。

错误日志对敏感字段、私钥和 URL 中的凭据、查询参数进行脱敏，不记录请求认证头或更新包签名正文。排查时可按同一个 `operation_id` 找到检查、下载、安装及恢复错误，结合 `runtime-*.log` 查看服务具体输出。

GitHub Actions 的准备、校验和失败日志包含运行编号、重试次数、源码提交、版本与构建目标。`.github/workflows/log-cleanup.yml` 每天 02:30 UTC 清理最后更新时间超过 14 天的已完成运行日志，近期重跑任务会保留。它只删除日志，不删除运行历史、Release、安装包或 GHCR 镜像；构建附件仍按工作流的 7 天期限过期。此定时任务在配置推送到默认分支后生效。

```sh
docker compose -f compose.dev.yaml run --rm --no-deps dev node --test scripts/desktop-logging.test.mjs scripts/desktop-github-updates.test.mjs
```
