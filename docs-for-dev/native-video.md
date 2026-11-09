# 聊天生成视频

旧版有两条独立链路；之前 Rust 只接入了设置页试生成。本次将两条链路接回聊天：

| 场景 | 迁移前 | 当前 Rust 状态 |
| --- | --- | --- |
| 讲解视频／动画课程 | `video_lesson` → 课程 TSX → 旁白 → Remotion 编译与帧检查 → 浏览器布局检查／修复 → 发布 HTML 播放器 | 新增原生课程能力、TSX 编译、隔离抽检 25 帧、旁白与字幕、会话播放器；未恢复旧版浏览器图形碰撞自动修复 |
| 聊天文字生成视频 | `videogen` 工具 → 活跃视频模型 → 提交任务、轮询、下载 → 保存工作区产物 → 展示 | 普通聊天可调用 `videogen`，在工具完成时保存 MP4，可以拖动播放进度或重新打开会话 |
| 各厂商视频协议 | 通用异步任务、DashScope、xAI 适配器 | 设置页与聊天共用 OpenAI 兼容、DashScope、xAI 和火山异步任务适配器 |

旧版 `video_lesson` 生成的是可以在浏览器播放的 Remotion 动画课程页面，源码注释明确说明它不在服务器编码 MP4。它不要求配置文生视频模型；不能用“去配置视频服务”替代恢复这条链路。

在普通聊天输入「做一个讲解能量守恒的动画视频」，或选择视频课程能力。已配置 TTS 时，课程按录音时长播放旁白和字幕；无 TTS 时生成字幕课程。制作 MP4 短片则在设置的多模态部分选定并应用视频生成模型，再在工具设置启用 `videogen`。聊天模型需支持工具调用。

## 运行与验证

课程工具包保存在 `packages/video-lesson`。Docker 开发环境和桌面构建都按锁文件安装依赖。独立运行 Rust 二进制时需提供 Node 与已安装依赖的工具包，可用 `DEEPTUTOR_VIDEO_RUNTIME` 和 `DEEPTUTOR_BROWSER_NODE` 指定位置。模型代码只在 QuickJS 中抽帧；交付页面通过 CSP 和隔离 iframe 在浏览器播放。帧检查能捕获运行时错误，但不能保证所有画面排版都理想。

视频字节与工具结果在同一个 SQLite 事务中提交，并按账号和工作区读取。会话删除时级联删除文件。失败时检查后端标准错误中的 `video_tool_error`，按 turn 和 operation 定位；日志不含源代码、提示词或签名链接。桌面运行日志沿用滚动保留策略。

容器内运行 `node scripts/native-video-acceptance.mjs` 可用临时数据目录、模拟聊天和视频服务回归；设置 `VIDEO_QA_CHROME` 为本地 Chromium 可执行文件可额外验证浏览器播放。它不调用真实商业模型。旧实现可在 Git 的 `74524faa0^` 中查看 `deeptutor/agents/video_lesson/` 和 `deeptutor/services/videogen/`。
