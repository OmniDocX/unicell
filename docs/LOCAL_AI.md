# 自配本机 AI

[项目概述](../README.zh-CN.md) · [本机 MCP](MCP.md)

基础编辑功能不依赖模型。使用 U AI 前，须在仓库根目录的 `.env.local`（已被 Git 忽略）中设置：

```dotenv
UNICELL_AI_BASE=https://your-provider.example/v1
UNICELL_AI_MODEL=your-model
UNICELL_AI_KEY=your-own-key
```

支持兼容 Chat Completions 的服务。远程服务须使用 HTTPS，HTTP 仅允许用于回环地址，例如 `http://127.0.0.1:11434/v1`。兼容服务如要求提供任意占位密钥，同样须填写 `UNICELL_AI_KEY`。

环境变量优先于本地文件；`UNICELL_AI_CONFIG_PATH` 可指定配置文件。默认仅查找当前目录，以及从 `server/` 启动时的仓库根目录，不查找相邻项目。模型与密钥由服务端读取，浏览器不接收密钥。

默认服务地址为 DashScope，默认模型为 `qwen3.7-flash`；应按实际拥有的服务修改。模型可用性、价格和能力由供应商决定。对 DashScope 的请求启用深度思考；单次输出最多 16384 个 token，本机进程最多同时处理 12 个 AI 请求。

执行 AI 操作时，问题和所选的工作簿上下文会发送至所配置的服务。应先检查预览，再应用写入；写入操作计入本机撤销历史。本仓库不附带账号、模型额度或云端密钥。
