# UniCell · 本机基础版

**这是纯国产 app project。** UniCell 是 OmniDoc 旗下自主开发的电子表格应用项目，提供中文界面、本机编辑与格式转换。

[English](README.en.md) · [OmniDoc 主站](https://omnidoc.top) · [项目合集](https://github.com/OmniDocX/omnidoc) · [UniPPT](https://github.com/OmniDocX/UniPPT) · [vecmeta](https://github.com/OmniDocX/vecmeta)

应用采用 Rust 引擎与浏览器界面，表格在本机服务中计算；首次构建完成后，基础编辑无需账号和联网。项目使用 IronCalc、KaTeX 等第三方开源组件，来源与独立许可证详见 [第三方声明](THIRD_PARTY_NOTICES.md)。国产应用项目定位不表示所有第三方依赖均为国产。

## 快速开始

准备 Rust 稳定工具链（支持 edition 2024）和现代浏览器。Windows 安装 Visual Studio C++ 构建工具；Linux 安装 C/C++ 编译器、pkg-config 与 OpenSSL 开发包。首次构建需要下载 Cargo 依赖。

```sh
git clone https://github.com/OmniDocX/unicell.git
cd unicell
cargo run --locked --manifest-path server/Cargo.toml
```

打开 **http://127.0.0.1:8143**。端口冲突时：

```sh
cargo run --locked --manifest-path server/Cargo.toml -- --port=8145
```

请在仓库根目录或 `server/` 内运行。`vecmeta/` 已随源码提供，无需下载其他私有仓库。发布构建加 `--release`；程序名称为 `opencell-server`。

## 保留的基础功能

- 单元格与公式编辑、数字格式、富文本、边框、合并、行列操作、多工作表、剪贴板、撤销和重做。
- 筛选、排序、冻结窗格、条件格式、数据验证、工作表保护与假设分析。
- XLSX / XLSM、CSV、UniDoc `.udoc`、UniCell HTML 导入导出，打印预览与分页。
- 图片、SVG、图表及部分原生 Office 对象编辑；导入文件中的复杂 OOXML 部件尽量保留。
- 本机文件保存、最近文件、浏览器恢复副本；可选 U AI 和 5 个本机 MCP 工具。

公开基础版已移除 R2、云存储、共享链接、多人协作、统一账号和托管服务额度系统。完整服务版能力不包含在此仓库中。

## 数据与兼容性

服务只绑定 `127.0.0.1`，浏览器会话的工作簿相互隔离。工作簿主要保存在服务内存中，重启或会话闲置 8 小时后会失效。请主动使用保存或另存为；浏览器恢复副本不能代替正式文件备份。

CSV 只导出当前工作表显示值，不能保留样式和公式结构。XLSM 可以携带宏部件，应用不执行 VBA。透视表、连接、SmartArt 等存在“原样保留、部分编辑”的边界，不能据此推断 Excel 全功能等价。详见 [功能与限制](docs/FEATURES.md)。

## 可选配置

### U AI

复制 `.env.example` 为 `.env.local`，填写自己的兼容 Chat Completions 服务、模型和密钥。仅检查 UniCell 自身配置，不读取相邻产品的凭据。未配置时，基础编辑照常工作。

使用 U AI 时，选择的工作簿上下文和问题会发送给你配置的模型服务，调用费用由该服务计收。详见 [本机 AI](docs/LOCAL_AI.md)。

### 数学公式与截图

普通单元格公式计算不需要 Python。把 LaTeX 转为 Office 数学对象时可安装：

```sh
python -m pip install -r tools/requirements-math.txt
```

服务通过 `UNICELL_PYTHON` 或系统 Python 调用转换器。HTML 对象截图需要本机 Chrome、Edge 或 Chromium；可用 `UNICELL_CHROMIUM` 指定可执行文件。

字体使用系统字体，可选自备且有使用权的字体库，见 [字体加载](FONT_LOADING.md)。

### 本机 MCP

启动后访问 `/docs/mcp-protocol.html`。`/mcp` 使用本机 HTTP POST 与 Cookie 工作簿会话，提供工作簿信息、读取单元格/区域、写入单元格、区域格式设置。详见 [MCP 接入](docs/MCP.md)。

## 开发与检查

```sh
cargo test --locked --manifest-path server/Cargo.toml
python -m unittest discover -s tools -p "test_*.py"
python tools/check_public_boundary.py
```

运行服务后，执行 `python tools/local_smoke.py --url http://127.0.0.1:8143` 进行隔离会话的导入、编辑、导出、MCP 和边界检查。Node.js 可用于 `node --check web/app.js`；浏览器扩展自测入口是 `/?test=auto`。

## 许可证与商业授权

自有代码采用 [OmniDoc 非商业源码许可 1.0](LICENSE)：符合条款的非商业使用免费；商业使用（包括中国境内企业）需事先取得书面授权。这是源码可见许可，不是 OSI 批准的开源许可。第三方组件保持各自授权。

商业咨询：**cc@omnidoc.top**，微信 **13184071590**，通常 48 小时内答复。见 [商业授权](docs/COMMERCIAL_LICENSE.md) 和 [许可范围](docs/LICENSING.md)。
