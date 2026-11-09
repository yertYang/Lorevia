# 聊天输入框语音

聊天输入框右侧麦克风提供“语音对话”和“语音输入”。语音输入将转写结果填入草稿；语音对话在结束录音后自动发送，沿用当前会话的模型、历史及上下文，完成回复后自动朗读。每轮点击说话，再点击结束录音。结束语音对话、关闭面板、切换会话或工作区会释放麦克风并停止朗读。原 `/voice` 页面跳转到 `/chat`，侧边栏不再提供独立入口。

## 默认服务器语音服务

在后端的环境配置中设置以下变量（Compose 从根目录 `.env` 读取）：

```dotenv
SPEECH_BASE_URL=http://lorevia-speech:8090/v1
SPEECH_API_KEY=<服务器 /srv/lorevia-speech/.env 中的 SPEECH_API_KEY>
SPEECH_STT_MODEL=sensevoice-small-int8
SPEECH_TTS_MODEL=kokoro-v1.1-int8
SPEECH_VOICE=zf_001
```

上述地址要求后端容器与语音服务共享 Docker 网络。在语音服务器宿主机直接运行后端时，可用 `http://127.0.0.1:18090/v1`。其他机器需要可达的私网地址或 SSH 隧道；容器的 `127.0.0.1` 指向容器本身。开发环境仍使用 `compose.dev.yaml`，不要启动生产 Compose 做开发。

环境变量改变后，执行 `docker compose -f compose.dev.yaml up -d` 使配置生效。仅重启不会更新现有容器的环境变量。不要将密钥写进前端、`NEXT_PUBLIC_` 变量或版本库。

未选择个人语音模型时，后端默认采用上述 SenseVoice/Kokoro 服务。已在“设置 → 语音”选定的模型优先，配置错误会直接报告，不会静默切换服务。`GET /api/voice/status` 只返回配置是否齐全及模型名，不返回密钥和服务地址；它不代表远端服务的健康检查。

麦克风使用前先检查配置。单段录音最多 120 秒、25 MiB，音频通过后端转发且不落盘。语音回复分段合成，每次最多发送 1500 个原始字符；服务本身要求每次合成最多 4096 字符。录音使用浏览器支持的 WebM、MP4 或 Ogg 格式，需要 HTTPS 或 localhost 安全上下文。

## 开发验证

语音端到端检查使用临时后端与模拟服务，不修改开发数据。先在容器内安装浏览器：

```sh
docker compose -f compose.dev.yaml exec -w /workspace/packages/browser-testing dev npx playwright install --with-deps --no-shell chromium
```

在 4 GB 开发容器中分两次运行，以降低页面、浏览器和编译缓存的同时占用：

```sh
docker compose -f compose.dev.yaml exec -e VOICE_QA_PHASE=desktop dev node scripts/composer-voice-acceptance.mjs
docker compose -f compose.dev.yaml exec -e VOICE_QA_PHASE=mobile dev node scripts/composer-voice-acceptance.mjs
```

截图生成在 `dist/composer-voice-acceptance/`。模拟测试通过不代表真实服务器可达，部署后还需使用真实服务验证。
