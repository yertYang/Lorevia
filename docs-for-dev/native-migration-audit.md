# Rust 迁移缺口核对

核对日期：2026-11-06。对照原生迁移提交 `74524faa0` 的上一版 Python 模块声明及实现，与当前 Rust 能力注册、工具执行器、模块状态和前端调用链。本表是代码核对，不代表每个旧服务都已使用真实账号回归。

## 视频迁移和使用方式

旧版有两条独立链路；之前 Rust 只接入了设置页试生成。本次将两条链路接回聊天：

| 场景 | 迁移前 | 当前 Rust 状态 |
| --- | --- | --- |
| 讲解视频／动画课程 | `video_lesson` → 课程 TSX → 旁白 → Remotion 编译与帧检查 → 浏览器布局检查／修复 → 发布 HTML 播放器 | 新增原生课程能力、TSX 编译、隔离抽检 25 帧、旁白与字幕、会话播放器；未恢复旧版浏览器图形碰撞自动修复 |
| 聊天文字生成视频 | `videogen` 工具 → 活跃视频模型 → 提交任务、轮询、下载 → 保存工作区产物 → 展示 | 普通聊天可调用 `videogen`，在工具完成时保存 MP4，可以拖动播放进度或重新打开会话 |
| 各厂商视频协议 | 通用异步任务、DashScope、xAI 适配器 | 设置页与聊天共用 OpenAI 兼容、DashScope、xAI 和火山异步任务适配器 |
| 数学动画 | `math_animator` 与 Manim 渲染 | 能力未注册，`renderer-manim` 模块未迁移 |

旧版 `video_lesson` 生成的是可以在浏览器播放的 Remotion 动画课程页面，源码注释明确说明它不在服务器编码 MP4。它不要求配置文生视频模型；不能用“去配置视频服务”替代恢复这条链路。

在普通聊天输入「做一个讲解能量守恒的动画视频」，或选择视频课程能力。已配置 TTS 时，课程按录音时长播放旁白和字幕；无 TTS 时生成字幕课程。制作 MP4 短片则在设置的多模态部分选定并应用视频生成模型，再在工具设置启用 `videogen`。聊天模型需支持工具调用。

## 其他已确认的缺口

| 范围 | 现状及迁移边界 |
| --- | --- |
| 图片生成 | 只迁了设置试生成；聊天 `imagegen` 和工作区媒体产物发布未恢复 |
| 可视化 | `visualize` / `submit_visualization` 未注册，模块未迁移 |
| 播客／音频概览 | `audio_overview` 未迁移；当前朗读、转写与实时语音属于其他能力，不能替代多段内容概览生成 |
| 视频学习 | `reading-media` 与 reading.media 不可用；当前原生阅读器支持文档和网页，视频字幕、学习标记及视频阅读对话未迁移 |
| 知识库 | 原生文档导入、引用和笔记已迁移；本次新增语义、混合和重排。GraphRAG、LightRAG 等外部引擎仍未接入 |
| 高级解析器 | Docling、LiteParse、MarkItDown、PyMuPDF4LLM 插件及原解析管理未迁移；不能据此说 PDF/Office 都不能读，原生已实现这些文件的基础解析 |
| 研究 | 原生研究流程、证据记录及发布已实现；旧版 `preprint`、`research_audit`、`research_lit`、`paper_search`、`knowledge_frontier` 专用工具未恢复 |
| 题库／掌握度 | 原生出题、作答、复习和学习路径已实现；旧 question_bank 管理、旧 practice 页面及旧 mastery 工具族未等量迁移 |
| 伙伴、书库 | 原模块仍未迁移 |
| 记忆 | 基础读写、来源图和需审核的整理已实现；旧 L1 浏览、语义冲突解决和自动语义合并仍不可用 |
| 外部资料集成 | 旧 Obsidian、MarginNote、IMA、Zotero 工具没有出现在原生工具注册中 |
| 通用执行与工作区文件 | 旧 exec、read_skill、load_tools、workspace_list/read/search/present/export 等没有在原生工具注册中恢复；这也影响“聊天生成文件再交付”的工作流 |

模块清单中旧 `research` / `questions` 未就绪，与新的 `native-study` 已就绪同时存在。迁移检查必须核对具体行为，不能把旧模块名称直接等同于整个功能缺失，也不能把新模块的 ready 当成旧功能已全部覆盖。

## 运行与验证

课程工具包保存在 `packages/video-lesson`。Docker 开发环境和桌面构建都按锁文件安装依赖。独立运行 Rust 二进制时需提供 Node 与已安装依赖的工具包，可用 `DEEPTUTOR_VIDEO_RUNTIME` 和 `DEEPTUTOR_BROWSER_NODE` 指定位置。模型代码只在 QuickJS 中抽帧；交付页面通过 CSP 和隔离 iframe 在浏览器播放。帧检查能捕获运行时错误，但不能保证所有画面排版都理想。

视频字节与工具结果在同一个 SQLite 事务中提交，并按账号和工作区读取。会话删除时级联删除文件。失败时检查后端标准错误中的 `video_tool_error`，按 turn 和 operation 定位；日志不含源代码、提示词或签名链接。桌面运行日志沿用滚动保留策略。

容器内运行 `node scripts/native-video-acceptance.mjs` 可用临时数据目录、模拟聊天和视频服务回归；设置 `VIDEO_QA_CHROME` 为本地 Chromium 可执行文件可额外验证浏览器播放。它不调用真实商业模型。旧实现可在 Git 的 `74524faa0^` 中查看 `deeptutor/agents/video_lesson/` 和 `deeptutor/services/videogen/`。
