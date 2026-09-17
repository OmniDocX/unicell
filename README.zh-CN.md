<div align="center">

<img src="web/unicell-logo.svg" width="72" height="72" alt="UniCell" />

# UniCell

**本机电子表格：从数据整理、公式计算到工作簿交付。**

[English](README.md) | **简体中文**

[官网](https://omnidoc.top/) · [在线体验](https://unicell.unidoc.top/) · [快速开始](#快速开始) · [文档](#文档) · [测评](benchmarks/README.zh-CN.md)

[![CI](https://github.com/OmniDocX/unicell/actions/workflows/ci.yml/badge.svg)](https://github.com/OmniDocX/unicell/actions/workflows/ci.yml)
[![License: PolyForm Noncommercial](https://img.shields.io/badge/license-PolyForm_Noncommercial-315EFB?style=flat-square)](LICENSE)
[![GitHub issues](https://img.shields.io/github/issues/OmniDocX/unicell?style=flat-square)](https://github.com/OmniDocX/unicell/issues)

</div>

UniCell 把熟悉的电子表格操作带到浏览器：导入 Excel 或 CSV，编辑多工作表，计算公式，设置样式，并将结果保存回本机文件。Rust 服务负责计算与文档处理，浏览器提供编辑、数据工具和打印界面。

**OmniDoc 自主研发的纯国产应用项目（app project）**，计算引擎采用 IronCalc，公式排版采用 KaTeX；第三方组件独立署名。

![UniCell spreadsheet editor](docs/images/editor.png)

<sub>公开本机版实际界面：演示数据、公式、数字格式和汇总计算。</sub>

## 核心能力

- **完整的日常编辑** — 多工作表、单元格与区域操作、行列调整、合并、富文本、撤销和重做。
- **公式与自动重算** — 基于 IronCalc 的公式引擎处理依赖计算；有界 SUM 已优化，随源码提供可复现测评。
- **数据整理** — 排序、筛选、冻结窗格、条件格式、数据验证与受支持的假设分析操作。
- **文件往返** — 支持 XLSX/XLSM、CSV、UDOC 和 UniCell HTML 导入导出，便于继续编辑与本机归档。
- **可视化与打印** — 处理图片、SVG、图表及受支持的 Office 对象，提供页面设置和浏览器打印。
- **AI 自动化** — 配置 U AI 辅助工作簿操作，或通过本机 MCP 读取数据、写入公式和设置格式。

## 快速开始

安装 Git、支持 edition 2024 的 Rust 稳定工具链和现代浏览器：

```sh
git clone https://github.com/OmniDocX/unicell.git
cd unicell
cargo run --release --locked --manifest-path server/Cargo.toml
```

访问 **http://127.0.0.1:8143**。修改端口可在命令后追加 `-- --port=8145`；请从仓库根目录或 `server/` 目录运行。

Windows 需 Visual Studio C++ 构建工具；Linux 需 C/C++ 工具链、pkg-config 和 OpenSSL 开发包。

## AI 与开发接入

**U AI**：复制 `.env.example` 为 `.env.local`，配置兼容 Chat Completions 的模型服务。基础编辑无需模型；启用 AI 后，所选上下文会发送至配置的服务。[配置说明](docs/LOCAL_AI.md)。

**MCP**：本机 `/mcp` 提供以下五个工具，使用当前 Cookie 工作簿会话：[协议与接入示例](docs/MCP.md)。

| 工具 | 用途 |
| --- | --- |
| `workbook_info` | 查看工作簿与工作表 |
| `read_cell` / `read_range` | 读取单元格与范围 |
| `write_cell` | 写入数值、文本和公式 |
| `format_range` | 设置区域格式 |

## 性能

<!-- BENCHMARK:START -->
有界 SUM 优化后，1 万行 CSV 导入与计算中位数从 **20.217 秒降至 0.661 秒，约快 31 倍**。每轮全部 2 万个公式及结果均核验正确。

| 操作 | 工作负载 | 中位数 |
| --- | --- | --- |
| CSV 导入与计算 | 10,000 行 / 20,000 公式 | **660.92 ms** |
| 批量编辑与重算 | 10,000 行 / 20,000 公式 | **120.68 ms** |

2026-09-16 · Windows 10 · Intel i7-1165G7 · 31.7 GiB · Rust release · 预热 2 次 / 测量 7 次。

[完整测评、原始数据与复现方法](benchmarks/README.zh-CN.md) — 以上为合成工作负载的本机测试，不含浏览器渲染，未与其他办公软件做速度对测。
<!-- BENCHMARK:END -->

## 版本与文件兼容性

本仓库提供单人本机版，不含 R2、云存储、共享编辑或统一账户。活动工作簿存放在服务内存中，需主动保存；服务重启或会话闲置八小时后，活动状态会失效。

XLSM 可保留受支持的宏部件，但不执行 VBA；CSV 导出显示值。透视表、SmartArt、外部连接等高级对象的编辑与保留范围见[功能说明](docs/FEATURES.md)。

## 文档

| 入口 | 内容 |
| --- | --- |
| [功能说明](docs/FEATURES.md) | 编辑、文件格式与高级对象 |
| [U AI](docs/LOCAL_AI.md) | 模型配置与数据处理 |
| [MCP](docs/MCP.md) | 本机协议与调用示例 |
| [字体加载](FONT_LOADING.md) | 本机字体与显示 |
| [第三方声明](THIRD_PARTY_NOTICES.md) | 依赖来源与许可证 |

## 参与开发

```sh
cargo test --locked --manifest-path server/Cargo.toml
python -m unittest discover -s tools -p "test_*.py"
python benchmarks/verify_results.py
```

服务运行后，可执行 `python tools/local_smoke.py --url http://127.0.0.1:8143` 检查本机 API。

## OmniDoc 产品与社区

[OmniDoc 主站](https://omnidoc.top/) · [UniPPT](https://github.com/OmniDocX/UniPPT) · [UniCell](https://github.com/OmniDocX/unicell) · [vecmeta](https://github.com/OmniDocX/vecmeta)

问题与建议请提交 [GitHub Issue](https://github.com/OmniDocX/unicell/issues)，附上复现步骤和可公开的最小示例。欢迎参与功能开发、兼容性改进和文档建设。

办公体验对标 Microsoft 365（Office 365）与 WPS Office；公开办公项目参照 ONLYOFFICE、Univer。[项目定位与能力对照](docs/COMPARISON.zh-CN.md)。

## 许可证与商业合作

自有代码采用 [PolyForm Noncommercial 1.0.0](LICENSE)。标准许可允许的非商业及特定机构用途免费；超出允许范围的商业用途，须[申请付费商业授权](docs/COMMERCIAL_LICENSE.md)并取得书面许可。

**商业联系：[cc@omnidoc.top](mailto:cc@omnidoc.top) · 微信：13184071590**

本项目采用源码可见许可（非 OSI 开源许可）。第三方组件及旧版本有效授权保持独立，详见[许可范围](docs/LICENSING.md)。
