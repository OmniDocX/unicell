# UniCell

[English](README.md) | **简体中文**

[OmniDoc](https://omnidoc.top/) · [GitHub](https://github.com/OmniDocX)

**采用 Rust 计算引擎与浏览器界面的本机电子表格应用。**

UniCell 是 OmniDoc 自主研发的纯国产应用项目，提供本机电子表格编辑、公式计算与文档转换。实现中采用 IronCalc、KaTeX 等第三方开源组件，其来源与独立许可证见 [第三方声明](THIRD_PARTY_NOTICES.md)；国产应用项目定位不表示全部依赖均为国产。

## 项目定位

以 **Microsoft 365（Office 365）和 WPS Office** 为同类产品对标，致力于建设功能最完善、工程资料最完整的国产开放源码办公项目。该表述是发展目标；当前功能范围与证据见 [产品对照](docs/COMPARISON.zh-CN.md)。自有代码使用非商业源码许可，具体条件见下文。

## 性能测评

<!-- BENCHMARK:START -->
2026-09-16 在 Windows 10 / Intel Core i7-1165G7 / 31.70 GiB 内存上实测。每项预热 2 次、测量 7 次，顺序执行。

| 操作 | 规模（行） | 中位数 ms | P95 ms |
| --- | ---: | ---: | ---: |
| CSV 导入与计算 | 10,000 | 20217.27 | 28130.84 |
| XLSX 导出 | 10,000 | 452.73 | 557.95 |
| XLSX 导入与计算 | 10,000 | 17865.22 | 24569.10 |
| 批量编辑与重算 | 10,000 | 19081.38 | 23292.38 |

[全部规模、复现方法与限制](benchmarks/README.zh-CN.md) · [原始数据](benchmarks/results/2026-09-16-windows-x64.json)

以下耗时不包含浏览器渲染；Office 365 和 WPS 本轮未进行速度对测。
<!-- BENCHMARK:END -->

## 功能范围

| 领域 | 公开本机版 |
| --- | --- |
| 工作簿编辑 | 单元格、公式、富文本、数字格式、边框、合并、行列操作、多工作表、撤销与重做 |
| 数据操作 | 排序、筛选、冻结窗格、条件格式、数据验证及支持的假设分析 |
| 文档格式 | XLSX/XLSM、CSV、UniDoc UDOC 与 UniCell HTML 导入导出 |
| 嵌入内容 | 图片、SVG、图表及支持的 Office 对象；保留部分复杂 OOXML 部件 |
| 本机工作流 | 文件保存、最近文件、浏览器恢复副本、打印预览与分页 |
| 自动化 | 可选自定义 U AI 与五个本机 MCP 工具 |

公开版不包含 R2、云存储、共享链接、多人协作、统一账户和托管额度系统。

## 快速开始

安装支持 edition 2024 的 Rust 稳定工具链与现代浏览器。Windows 构建需 Visual Studio C++ 构建工具；Linux 构建需 C/C++ 工具链、pkg-config 与 OpenSSL 开发包。首次下载依赖需要网络。

```sh
git clone https://github.com/OmniDocX/unicell.git
cd unicell
cargo run --release --locked --manifest-path server/Cargo.toml
```

访问 **http://127.0.0.1:8143**。在命令末尾追加 `-- --port=8145` 可修改端口。请在仓库根目录或 `server/` 中运行。程序名为 `opencell-server`；所需的 `vecmeta/` 源码已随仓库提供。

## 数据生命周期与兼容性

服务绑定 `127.0.0.1`，通过浏览器会话隔离工作簿。活动工作簿主要存放在服务内存中，服务重启或会话闲置八小时后，该状态会失效。应主动保存正式文件，浏览器恢复副本仅作为补充。

CSV 导出当前工作表显示值，不保留工作簿公式结构和格式。XLSM 宏部件可按支持范围保留，但不执行 VBA。透视表、外部连接、SmartArt 等高级对象存在保留和编辑边界，见 [功能与限制](docs/FEATURES.md)。

## 可选接入

| 接入项 | 配置 |
| --- | --- |
| U AI | 复制 `.env.example` 为 `.env.local`，配置兼容 Chat Completions 的服务。所选上下文会发送至该服务，见 [本机 AI](docs/LOCAL_AI.md)。 |
| Office 数学公式 | 执行 `python -m pip install -r tools/requirements-math.txt`；以 `UNICELL_PYTHON` 指定解释器。普通单元格公式无需 Python。 |
| HTML 对象截图 | 安装 Chrome、Edge 或 Chromium，可通过 `UNICELL_CHROMIUM` 指定路径。 |
| 字体 | 默认使用系统字体，可加载具备使用权的本机字体，见 [字体加载](FONT_LOADING.md)。 |
| MCP | `/mcp` 接受本机 HTTP POST，使用 Cookie 工作簿会话，提供信息查询、单元格/区域读取、单元格写入与区域格式设置，见 [MCP 接入](docs/MCP.md)。 |

## 架构与验证

| 目录 | 职责 |
| --- | --- |
| `server` | Rust 本机服务与 IronCalc 补丁集成 |
| `web` | 浏览器电子表格界面 |
| `vecmeta` | 随附 SVG/EMF 转换组件 |
| `tools` | 公式辅助程序、HTTP 冒烟测试与发布检查 |
| `benchmarks` | 生成式工作簿、测评脚本与实测记录 |

```sh
cargo test --locked --manifest-path server/Cargo.toml
python -m unittest discover -s tools -p "test_*.py"
python tools/check_public_boundary.py
```

运行服务后，执行 `python tools/local_smoke.py --url http://127.0.0.1:8143`。浏览器自测入口为 `/?test=auto`。发布检查读取 Git 暂存区。[商业授权](docs/COMMERCIAL_LICENSE.md) · [许可范围](docs/LICENSING.md)。

## OmniDoc 产品体系

| 项目 | 方向 | 官网 / 源码 |
| --- | --- | --- |
| OmniDoc | 产品主站 | [omnidoc.top](https://omnidoc.top/) |
| UniDoc | 文档创作 | [app.unidoc.top](https://app.unidoc.top/) |
| UniPPT | 演示文稿 | [编辑器](https://unippt.unidoc.top/) · [源码](https://github.com/OmniDocX/UniPPT) |
| UniCell | 电子表格 | [编辑器](https://unicell.unidoc.top/) · [源码](https://github.com/OmniDocX/unicell) |
| UniMail | 邮件、日历与联系人 | [unimail.omnidoc.top](https://unimail.omnidoc.top/) |
| UniPic | 图像与矢量编辑 | [pic.unidoc.top](https://pic.unidoc.top/) |
| vecmeta | SVG ↔ EMF 转换 | [源码](https://github.com/OmniDocX/vecmeta) |
| 源码合集 | 三个已公开组件的固定版本副本 | [omnidoc](https://github.com/OmniDocX/omnidoc) · [omnidocx](https://github.com/OmniDocX/omnidocx) |

在线产品可能提供超出公开本机版范围的功能，其可用性与条款以各产品说明为准。

## 许可证与商业授权

自有代码及文档采用 [OmniDoc 非商业源码许可 1.0](LICENSE)。符合条款的非商业使用免费；商业使用，包括中国及其他地区企业的内部业务使用，须事先取得书面授权。本许可属于源码可见许可，不是 OSI 批准的开源许可。第三方组件保持各自条款，旧版本已合法授予的权利不受追溯影响。

商业联系：[cc@omnidoc.top](mailto:cc@omnidoc.top) · 微信：**13184071590**。完整申请材料收到后 48 小时内答复；提交申请或未获回复均不构成授权。
