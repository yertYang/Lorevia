# 原生高级检索

知识库页面展开「高级检索」，可保存关键词、语义或混合检索模式。聊天和原生研究中的 `rag` 工具使用同一份知识库设置，模型不能自行切换工作区或覆盖这些设置。

1. 在设置中选择可用的嵌入模型，使用兼容 OpenAI 的 `/embeddings` 接口。模型维度可留空自动识别；启用发送维度时必须填写维度。
2. 导入文档后点击「构建／重建向量索引」，等待索引就绪。
3. 保存语义或混合检索模式。可开启「使用当前语言模型重排」，也可筛选来源文件、选择返回 3、5 或 10 段。

关键词检索无需模型服务。语义检索将原生切分后的文档段落及查询发送给已配置的嵌入服务；重排将查询和候选段落发送给当前默认语言模型，可能产生费用。导入不会自动触发这些付费请求。

## 排序及索引

- 向量归一化后使用余弦相似度，持久化于同一账号的 SQLite 数据库。当前上限为每知识库 20,000 段、每向量 8,192 维，适合本地中小规模资料库。
- 混合检索融合 FTS5 BM25 与向量候选的名次，使用 RRF（`k=60`），默认取 30 个候选，可设置 10–50 个。最终返回最多 10 段。
- 可选语言模型重排只允许返回候选段落编号的完整排列；重复、遗漏、越界或无效 JSON 会明确报错。原文、来源哈希和引用链接不由模型生成。
- 模型身份包括配置 ID、接口地址、模型名称、维度及额外请求头的哈希；密钥轮换本身不会使索引失效。换模型或修改、添加、删除文档后必须重建。未变化的段落复用已发布的同模型向量。
- 索引以 generation 构建并原子发布，失败时保留上一代。对已过期的上一代不会继续进行语义搜索，也不会悄悄回退到关键词结果。
- 网络请求不占用数据库事务。全服务同时只构建一个向量索引，单次请求最多 120 秒、整个构建最多 15 分钟。后台构建不依赖页面连接；进程意外退出后，3 分钟租约过期会显示中断，可重试。
- 引用沿用不可变文件版本及准确行号。替换文件保留旧引用；用户明确删除文件时，原有删除行为会一并删除其来源版本和向量。

这里实现的是原生高级检索。旧版 GraphRAG / LightRAG / LlamaIndex 外部引擎没有被冒充为已接入，能力清单中的 `advanced_providers` 仍为 `false`。

## 接口

接口继续使用 `dt_workspace` / `workspace_id` 工作区作用域，两者同时给出时必须一致。

| 请求 | 说明 |
| --- | --- |
| `GET /api/knowledge-bases/{kb}/retrieval` | 已保存设置、索引状态、进度、嵌入模型及最近错误 |
| `PUT /api/knowledge-bases/{kb}/retrieval` | 保存 `{mode, rerank, candidate_k}`；模式为 `lexical`、`vector` 或 `hybrid` |
| `POST /api/knowledge-bases/{kb}/reindex` | 启动后台索引，返回 `status: building` 及 `operation_id`，随后轮询状态 |
| `POST /api/knowledge-bases/{kb}/search` | `{query, top_k?, source_path?, mode?, rerank?}`；省略模式和重排时使用已保存设置 |

检索响应保留原文、引用、来源哈希和行号，增加实际使用的 `retrieval_mode`、`rerank`；混合结果含 `lexical_score` / `vector_score`，重排结果另含 `retrieval_score` / `rerank_position`。

## 错误定位与验证

索引或检索失败时输出一条结构化错误，包含 UTC 时间、`operation_id`、阶段、账号内工作区、知识库、耗时、错误码和 HTTP 状态。响应也附带操作编号，最近索引错误及编号保存在状态接口中。不会记录查询、文档原文、密钥或上游响应正文。

桌面版由现有运行日志接收这些错误，按天及大小轮转，启动时和每小时清理，最多保留 14 天，总预算 64 MB；详见 [桌面发布与日志](desktop-releases.md)。开发容器可使用 `docker compose -f compose.dev.yaml logs --tail=100 dev` 查看。

```sh
docker compose -f compose.dev.yaml exec -T dev cargo test --locked -p deeptutor-server
docker compose -f compose.dev.yaml exec -T -w /workspace/web dev npx tsc --noEmit
```

合成提供方测试覆盖无共同关键词的语义召回、混合排序、真实流式语言模型重排、工作区隔离、聊天工具接入、索引复用、模型变化、文档变化、失败恢复、异常向量及 HTTP 后台构建流程。合成测试不代表已验证用户的具体商业模型或第三方服务可用性。
