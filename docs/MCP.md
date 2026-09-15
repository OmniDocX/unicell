# 本机 MCP

地址：`http://127.0.0.1:8143/mcp`。仅 HTTP POST，JSON 响应，协议版本 `2025-06-18`。不提供 SSE、OAuth 或公网工具服务；客户端需要保留响应的 `unicell_session` Cookie 才能连续操作同一工作簿。新 Cookie 会话得到独立的默认工作簿，不自动连接到另一个浏览器会话。

| 工具 | 参数 |
| --- | --- |
| `workbook_info` | `{}` |
| `read_cell` | `sheet`（从 0 开始的索引或名称）、`cell`（例如 A1） |
| `read_range` | `sheet`、`range`（例如 A1:C20，最多 1000×100） |
| `write_cell` | `sheet`、`cell`、`value`（字符串，公式以 = 开头） |
| `format_range` | `sheet`、`range`、`path`、`value`，支持的样式见 `/mcp-tools.json` |

客户端先发送 `initialize`，再发 `notifications/initialized`，随后 `tools/list` 或 `tools/call`。初始化和列举使用标准 JSON-RPC `result`；工具输出位于 `result.content` 与 `result.structuredContent`。工具错误有 `isError: true`。所有写入受工作表保护规则约束，进入应用撤销历史。不接收任意磁盘路径。

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"local-client","version":"1"}}}
```

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"write_cell","arguments":{"sheet":1,"cell":"A1","value":"=3*4"}}}
```

本项目已检查 HTTP JSON 协议与 Cookie 连续性，未保证所有第三方 MCP 客户端自动管理该 Cookie。协议依据：[MCP lifecycle 2025-06-18](https://modelcontextprotocol.io/specification/2025-06-18/basic/lifecycle)。
