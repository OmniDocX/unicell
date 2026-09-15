<h1 align="center">vecmeta</h1>
<p align="center"><strong>原生 Rust SVG ↔ EMF 矢量转换引擎</strong></p>
<p align="center">OmniDoc 旗下矢量格式转换组件，提供双向转换、统一场景表示与结构化兼容性报告。</p>
<p align="center"><a href="https://github.com/OmniDocX/vecmeta/actions/workflows/verify.yml"><img alt="Verify" src="https://github.com/OmniDocX/vecmeta/actions/workflows/verify.yml/badge.svg"></a> <img alt="Rust" src="https://img.shields.io/badge/Core-Rust-222222"> <a href="LICENSE"><img alt="Non-commercial license" src="https://img.shields.io/badge/License-Non--Commercial%20%2B%20Commercial-orange"></a></p>

<p align="center"><strong>简体中文</strong> · <a href="README_EN.md">English</a></p>
<p align="center"><a href="https://github.com/OmniDocX/vecmeta">项目源码</a> · <a href="https://omnidoc.top/">OmniDoc 主站</a> · <a href="https://unippt.unidoc.top/">UniPPT</a> · <a href="https://pic.unidoc.top/">UniPic</a> · <a href="docs/COMMERCIAL_LICENSE.md">商业授权</a></p>

---

## 项目概述

vecmeta 是 OmniDoc 自研的 SVG 与 EMF 双向转换引擎，以 Rust 库和 `emfsvg` 命令行工具形式交付。组件通过统一的矢量中间表示连接两种格式，适用于文档编辑器、演示文稿引擎、矢量资源处理和格式往返验证。

EMF 二进制记录读写、图元回放、SVG 解析及矢量输出由 Rust 实现，转换过程无需调用 PowerPoint、LibreOffice 或 C 版 libemf2svg。文本可转换为矢量字形轮廓；输出轮廓保留矢量几何，不保留文本编辑语义。

| 核心能力 | 技术说明 |
| --- | --- |
| **原生 EMF 读写** | 直接处理记录、状态、变换、画笔、画刷、路径和图元 |
| **SVG ↔ EMF 双向链路** | 同一 Scene IR 连接两个格式，便于集成和检查 |
| **字形轮廓** | 通过 fontdb / ttf-parser 将文字转换为填充路径 |
| **渐变与透明度传递** | 通过自有输出的嵌入 EMF+ 绘制信息恢复线性渐变与 alpha |
| **源数据封装** | `--lossless` 携带原始字节或 SVG 文本，支持反向精确恢复 |
| **CSS 与引用解析** | 处理受支持的 `<style>`、继承、优先级及 `<use>` 引用 |
| **兼容性报告** | 返回被跳过内容的警告及嵌入源数据恢复状态，供调用方评估转换完整性 |

## 目录

[快速开始](#快速开始) · [命令行](#命令行) · [Rust 集成](#rust-集成) · [精度与支持范围](#精度与支持范围) · [OmniDoc 旗下业务](#omnidoc-旗下业务) · [许可](#许可与商业使用)

## 快速开始

构建环境需安装 Rust 与 Cargo。持续集成使用 Rust 1.88；文本轮廓转换要求运行环境安装相应字体。

```sh
git clone https://github.com/OmniDocX/vecmeta.git
cd vecmeta
cargo build --release --locked
cargo test --workspace --locked
cargo run --release -p emfsvg-cli -- --help
```

构建产物为 `target/release/emfsvg`（Windows 为 `emfsvg.exe`）。依赖通过 Cargo 管理，不需要安装 Office 或 C 转换库。

## 命令行

```sh
# EMF → SVG
cargo run --release -p emfsvg-cli -- to-svg drawing.emf -o drawing.svg

# SVG → EMF
cargo run --release -p emfsvg-cli -- to-emf drawing.svg -o drawing.emf

# 嵌入原始数据，支持反向转换时精确恢复
cargo run --release -p emfsvg-cli -- to-emf drawing.svg -o archive.emf --lossless

# 检查往返转换的几何一致性
cargo run --release -p emfsvg-cli -- roundtrip drawing.emf --tolerance 1e-6
```

`--verbose` 输出被跳过记录的信息；`--scale` 控制 SVG → EMF 的精度上限。完整参数定义见 `--help`。

## Rust 集成

依赖工作区内的 `emf2svg`、`svg2emf` 包即可调用库接口：

```rust
use emf2svg::{emf_to_svg_with, Emf2SvgOptions};
use svg2emf::{svg_to_emf, EmitOptions};

let source = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><circle cx="50" cy="50" r="25"/></svg>"#;
let emf = svg_to_emf(source, EmitOptions {
    lossless: true,
    ..Default::default()
}).expect("SVG to EMF");
let recovered = emf_to_svg_with(&emf, Emf2SvgOptions { lossless: true }).expect("EMF to SVG");
assert_eq!(source, recovered);
```

`emf_to_svg_with_report` 与 `svg_to_emf_with_report` 返回转换结果、警告信息及嵌入源数据恢复状态。基础转换接口保持兼容。UniPPT 内置组件采用独立版本快照，集成时应以对应快照的 API 定义为准。

| Crate | 职责 |
| --- | --- |
| `vector-ir` | Scene、Element、路径、矩阵、颜色与绘制样式 |
| `emf-core` | EMF 二进制记录、读写和嵌入绘制信息 |
| `glyph2path` | 字体查找与字形轮廓 |
| `emf2svg` | EMF 回放 → Scene → SVG |
| `svg2emf` | SVG 解析 → Scene → EMF |
| `emfsvg-cli` | 命令行与往返回归测试 |

## 精度与支持范围

### 几何一致性与源数据恢复

1. **矢量几何往返**：在支持的图元和规范化表示范围内检查坐标、变换及样式；整型量化、圆弧转贝塞尔等处理可能引入表示差异。
2. **源数据字节恢复**：`--lossless` 将原始数据嵌入目标格式，供反向转换时精确恢复。该机制不保证第三方软件的渲染一致性，也不将中间图形修改自动合并至嵌入的原始数据。

### 格式支持与限制

- 支持常用路径、基础图形、矩阵变换、填充/描边、线性渐变及受支持的 SVG CSS。
- 位图、滤镜、裁剪区域等不支持完整矢量转换的内容可能被跳过，调用方应检查转换警告。
- 径向渐变几何按线性近似处理；传统 EMF 查看器可能仅显示渐变平均色，并忽略透明度。
- 文字依赖系统字体，不提供完整的复杂文字整形、双向排版或所有字体特性。
- 第三方 Office 应用及浏览器中的像素级渲染一致性需独立验证。

## 测试与发布

`cargo test --workspace --locked` 执行库测试及自包含 SVG/EMF 往返测试。依赖外部 EMF 样本库的四项测试标记为 `ignored`，需显式启用；公开发行包不包含单独授权的历史测试素材。

使用已获授权的外部样本库时，通过环境变量指定包含 `emf/`、`emf-corrupted/`、`emf-ea/` 的目录：

```powershell
$env:VECMETA_CORPUS_DIR = "C:\path\to\tests\resources"
cargo test -p emfsvg-cli --test roundtrip -- --ignored
```

说明见 [来源与发行边界](docs/PROVENANCE.md)。第三方素材授权与原创代码许可分别处理。

## OmniDoc 旗下业务

OmniDoc 提供文档处理、内容创作、演示文稿、电子表格、邮件管理及图像处理产品，并提供配套的格式转换组件。

| 产品 / 组件 | 业务方向 | 官方网站 / 源码 |
| --- | --- | --- |
| OmniDoc | 主站、文档服务与账户基础设施 | [omnidoc.top](https://omnidoc.top/) |
| UniDoc | 文档创作与编辑 | [app.unidoc.top](https://app.unidoc.top/) |
| UniPPT | 原生演示文稿与 AI 编排 | [unippt.unidoc.top](https://unippt.unidoc.top/) · [源码](https://github.com/OmniDocX/UniPPT) |
| UniCell | 浏览器电子表格 | [unicell.unidoc.top](https://unicell.unidoc.top/) |
| UniMail | 多账户邮件、日历、联系人与 AI 邮件辅助 | [unimail.omnidoc.top](https://unimail.omnidoc.top/) |
| UniPic | 图片与矢量编辑 | [pic.unidoc.top](https://pic.unidoc.top/) |
| vecmeta | 原生 Rust SVG ↔ EMF 引擎 | [源码与使用说明](https://github.com/OmniDocX/vecmeta) |
| PolyglotPDF | 多语种电子书与 PDF 翻译 | [源码](https://github.com/OmniDocX/PolyglotPDF) |

各产品的部署方式、账户要求及许可条款以对应官方文档为准。vecmeta 以 Rust 库和命令行工具形式交付。

## 许可与商业使用

**非商业用途免费；商业使用须事先取得书面许可。** 当前自有源码采用 [OmniDoc 非商业源码许可 1.0](LICENSE)，属于源码可见许可，不是 MIT/GPL 或 OSI 开源许可。

中国公司及其他国家或地区的企业用于内部业务、商业产品、SaaS/API、客户交付和商业集成时，均须取得授权。

- 邮箱：[cc@omnidoc.top](mailto:cc@omnidoc.top)
- 微信：添加手机号 **13184071590**
- 完整申请收到后 **48 小时内完成审核并答复**；审核不等于自动批准，超时未回复不构成授权。

详见 [商业申请](docs/COMMERCIAL_LICENSE.md)。本次许可不撤销旧版本依法授予的权利，不替代第三方依赖及测试资产的原有协议。
