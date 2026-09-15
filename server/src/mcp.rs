//! Local, session-scoped MCP HTTP tools. No filesystem paths are accepted.
use super::*;
use serde_json::{Value, json};

const PROTOCOL: &str = "2025-06-18";

pub fn metadata(path: &str) -> Option<Value> {
    match path {
        "/mcp-tools.json" | "/docs/mcp-tools.json" => Some(tool_list()),
        "/mcp-protocol.html" | "/docs/mcp-protocol.html" => None,
        _ => None,
    }
}

pub fn protocol_document() -> Resp {
    let html = include_str!("../../web/docs/mcp-protocol.html");
    tiny_http::Response::from_string(html).with_header(
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
            .unwrap(),
    )
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
}
fn tool_list() -> Value {
    json!({"tools":[
        tool("workbook_info","读取当前工作簿的名称和工作表。",json!({}),&[]),
        tool("read_range","读取一个有限的工作表区域；使用 sheet 名称或索引和 A1 range。",json!({"sheet":{"type":["string","integer"]},"range":{"type":"string","description":"A1:C20"}}),&["sheet","range"]),
        tool("read_cell","读取单个单元格的显示值、公式和样式。",json!({"sheet":{"type":["string","integer"]},"cell":{"type":"string","description":"B2"}}),&["sheet","cell"]),
        tool("write_cell","写入单个单元格；应用到当前本机会话。",json!({"sheet":{"type":["string","integer"]},"cell":{"type":"string"},"value":{"type":"string"}}),&["sheet","cell","value"]),
        tool("format_range","给区域应用一个有限的 Excel 样式属性；应用到当前本机会话。",json!({"sheet":{"type":["string","integer"]},"range":{"type":"string"},"path":{"type":"string","enum":["font.b","font.i","font.u","font.color","fill.color","alignment.horizontal","alignment.vertical","alignment.wrap_text"]},"value":{"type":"string"}}),&["sheet","range","path","value"]),
    ]})
}

fn rpc_result(id: &Value, value: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":value})
}
fn result(id: &Value, value: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":value.to_string()}],"structuredContent":value}})
}
fn error(id: Option<&Value>, code: i64, message: &str) -> Value {
    let mut value = json!({"jsonrpc":"2.0","error":{"code":code,"message":message}});
    value["id"] = id.cloned().unwrap_or(Value::Null);
    value
}
fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a Value, String> {
    args.get(key).ok_or_else(|| format!("缺少参数 {key}"))
}
fn sheet(args: &Value) -> Result<u32, String> {
    let value = arg(args, "sheet")?;
    if let Some(n) = value.as_u64() {
        return u32::try_from(n).map_err(|_| "sheet 超出范围".into());
    }
    let text = value.as_str().ok_or("sheet 必须是名称或索引")?;
    if text.len() > 128 || text.chars().any(char::is_control) {
        return Err("sheet 名称无效".into());
    }
    let names = arg(args, "__names")?
        .as_array()
        .ok_or("sheet 名称无法解析")?;
    names
        .iter()
        .position(|item| item.as_str() == Some(text))
        .map(|i| i as u32)
        .ok_or_else(|| "找不到工作表".to_string())
}
fn cell(value: &str) -> Result<(i32, i32), String> {
    ai_context::parse_cell(value).ok_or("cell 必须是有效的 A1 地址".into())
}
fn range(value: &str) -> Result<(i32, i32, i32, i32), String> {
    let parsed = ai_context::parse_range(value).ok_or("range 必须是有效的 A1 区域")?;
    if parsed.r1 - parsed.r0 > 999 || parsed.c1 - parsed.c0 > 99 {
        return Err("读取区域过大（最多 1000×100）".into());
    }
    Ok((parsed.r0, parsed.c0, parsed.r1, parsed.c1))
}

pub fn handle(st: &mut AppState, body: &[u8]) -> Result<Resp, String> {
    if body.len() > 512 * 1024 {
        return Ok(json_bytes_response(
            error(None, -32600, "MCP 请求过大").to_string().into_bytes(),
            413,
        ));
    }
    let request: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => {
            return Ok(json_bytes_response(
                error(None, -32700, "Invalid JSON").to_string().into_bytes(),
                400,
            ));
        }
    };
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Ok(json_bytes_response(
            error(None, -32600, "JSON-RPC 2.0 required")
                .to_string()
                .into_bytes(),
            400,
        ));
    }
    let id = request.get("id");
    let method = request.get("method").and_then(Value::as_str);
    let Some(method) = method else {
        return Ok(json_bytes_response(
            error(id, -32600, "MCP method 缺失")
                .to_string()
                .into_bytes(),
            400,
        ));
    };
    // Notifications never invoke tools or mutate workbooks.
    if id.is_none() {
        return Ok(json_bytes_response(Vec::new(), 202));
    }
    let response = match method {
        "initialize" => rpc_result(
            id.unwrap_or(&Value::Null),
            json!({"protocolVersion":PROTOCOL,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"UniCell","version":"1"}}),
        ),
        "notifications/initialized" | "ping" => {
            if id.is_some() {
                rpc_result(id.unwrap(), json!({}))
            } else {
                return Ok(json_bytes_response(Vec::new(), 202));
            }
        }
        "tools/list" => {
            if id.is_some() {
                rpc_result(id.unwrap(), tool_list())
            } else {
                return Ok(json_bytes_response(Vec::new(), 202));
            }
        }
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let mut args = request
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !args.is_object() {
                return Ok(json_bytes_response(
                    error(id, -32602, "arguments must be an object")
                        .to_string()
                        .into_bytes(),
                    200,
                ));
            }
            let names = st.model.get_model().workbook.get_worksheet_names();
            args["__names"] = json!(names);
            match call_tool(st, name, &args) {
                Ok(value) => result(id.unwrap(), value),
                Err(message) => rpc_result(
                    id.unwrap(),
                    json!({"isError":true,"content":[{"type":"text","text":message}]}),
                ),
            }
        }
        _ => error(id, -32601, "MCP 方法或工具不存在"),
    };
    Ok(json_bytes_response(response.to_string().into_bytes(), 200))
}

fn call_tool(st: &mut AppState, name: &str, args: &Value) -> Result<Value, String> {
    let names = st.model.get_model().workbook.get_worksheet_names();
    match name {
        "workbook_info" => Ok(json!({"fileName":st.file_name,"sheets":names})),
        "read_cell" => {
            let sh = sheet(args)?;
            let (row, col) = cell(arg(args, "cell")?.as_str().ok_or("cell 必须是字符串")?)?;
            let q = format!("sheet={sh}&row={row}&col={col}");
            let response = api_cell(st, &q)?;
            let mut bytes = Vec::new();
            response
                .into_reader()
                .read_to_end(&mut bytes)
                .map_err(|_| "读取单元格失败")?;
            serde_json::from_slice(&bytes).map_err(|_| "读取单元格响应无效".into())
        }
        "read_range" => {
            let sh = sheet(args)?;
            let text = arg(args, "range")?.as_str().ok_or("range 必须是字符串")?;
            let (r0, c0, r1, c1) = range(text)?;
            let q = format!("sheet={sh}&r0={r0}&c0={c0}&r1={r1}&c1={c1}");
            let response = api_view(st, &q)?;
            let mut bytes = Vec::new();
            response
                .into_reader()
                .read_to_end(&mut bytes)
                .map_err(|_| "读取区域失败")?;
            serde_json::from_slice(&bytes).map_err(|_| "读取区域响应无效".into())
        }
        "write_cell" => {
            let sh = sheet(args)?;
            let (row, col) = cell(arg(args, "cell")?.as_str().ok_or("cell 必须是字符串")?)?;
            let value = arg(args, "value")?.as_str().ok_or("value 必须是字符串")?;
            if value.len() > 100_000 {
                return Err("value 过长".into());
            }
            let response = handle_api_with_history(
                st,
                "/api/input",
                "",
                &serde_json::to_vec(&json!({"sheet":sh,"row":row,"col":col,"value":value}))
                    .unwrap(),
            )?;
            let mut bytes = Vec::new();
            response
                .into_reader()
                .read_to_end(&mut bytes)
                .map_err(|_| "写入失败")?;
            serde_json::from_slice(&bytes).map_err(|_| "写入响应无效".into())
        }
        "format_range" => {
            let sh = sheet(args)?;
            let text = arg(args, "range")?.as_str().ok_or("range 必须是字符串")?;
            let (r0, c0, r1, c1) = range(text)?;
            let path = arg(args, "path")?.as_str().ok_or("path 必须是字符串")?;
            if !matches!(
                path,
                "font.b"
                    | "font.i"
                    | "font.u"
                    | "font.color"
                    | "fill.color"
                    | "alignment.horizontal"
                    | "alignment.vertical"
                    | "alignment.wrap_text"
            ) {
                return Err("不支持的格式属性".into());
            }
            let value = arg(args, "value")?.as_str().ok_or("value 必须是字符串")?;
            let response = handle_api_with_history(
                st,
                "/api/style",
                "",
                &serde_json::to_vec(
                    &json!({"sheet":sh,"r0":r0,"c0":c0,"r1":r1,"c1":c1,"path":path,"value":value}),
                )
                .unwrap(),
            )?;
            let mut bytes = Vec::new();
            response
                .into_reader()
                .read_to_end(&mut bytes)
                .map_err(|_| "格式设置失败")?;
            serde_json::from_slice(&bytes).map_err(|_| "格式响应无效".into())
        }
        _ => Err("未知 MCP 工具".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_contract_is_fine_grained() {
        let list = tool_list();
        let tools = list["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 5);
        assert!(
            tools
                .iter()
                .all(|t| t["inputSchema"]["additionalProperties"] == false)
        );
        assert!(!list.to_string().contains("token"));
    }
    fn response_json(response: Resp) -> Value {
        let mut bytes = Vec::new();
        response.into_reader().read_to_end(&mut bytes).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    #[test]
    fn protocol_handshake_and_local_writes_have_correct_results_and_history() {
        let mut state = AppState::new();
        let init = response_json(handle(&mut state,br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#).unwrap());
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL);
        let list = response_json(
            handle(
                &mut state,
                br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            )
            .unwrap(),
        );
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 5);
        let write = br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"write_cell","arguments":{"sheet":0,"cell":"A1","value":"=3*4"}}}"#;
        let result = response_json(handle(&mut state, write).unwrap());
        assert_eq!(result["result"]["structuredContent"]["ok"], true);
        assert_eq!(state.model.get_formatted_cell_value(0, 1, 1).unwrap(), "12");
        assert_eq!(state.app_undo.len(), 1);
        assert!(state.undo_application().unwrap());
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "");
        assert_eq!(handle(&mut state,br#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"write_cell","arguments":{"sheet":0,"cell":"A1","value":"hidden"}}}"#).unwrap().status_code().0,202);
        assert_eq!(state.model.get_cell_content(0, 1, 1).unwrap(), "");
        let invalid = response_json(
            handle(
                &mut state,
                br#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"arguments":[]}}"#,
            )
            .unwrap(),
        );
        assert_eq!(invalid["error"]["code"], -32602);
    }
    #[test]
    fn range_is_bounded() {
        assert!(range("A1:C100").is_ok());
        assert!(range("A1:C1001").is_err());
        assert!(cell("B2").is_ok());
        assert!(cell("A0").is_err());
    }
}
