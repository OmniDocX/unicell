# 同类产品对照

[English](COMPARISON.md) · [项目概述](../README.zh-CN.md)

## 对比对象与范围

**OmniDoc 的同类对比对象是 Microsoft 365（Office 365）和 WPS Office。** UniPPT 对应演示文稿工作流，UniCell 对应电子表格工作流；vecmeta 是底层矢量转换库与命令行组件。

项目以建设功能最完善、工程资料最完整的国产开放源码办公项目为目标。这是项目的发展愿景，尚未形成经独立评测确认的市场排名。当前公开版本覆盖下表所列组件，不代表已经提供 Microsoft 365 或 WPS 全部应用的替代实现。

## 能力矩阵

资料核对日期：2026-09-16。竞品列依据官方产品资料整理，属于功能范围对照；不代表已完成兼容性或性能实测。实际功能取决于版本、平台、地区和订阅方案。

| 维度 | OmniDoc 公开基础版 | Microsoft 365（Office 365） | WPS Office |
| --- | --- | --- | --- |
| 演示文稿 | UniPPT：浏览器编辑、原生 PPTX 对象、本机导入导出 | PowerPoint 桌面与网页版 | WPS 演示应用 |
| 电子表格 | UniCell：浏览器编辑、公式计算、XLSX/CSV 导入导出 | Excel 桌面与网页版 | WPS 表格应用 |
| 产品覆盖 | 本合集包含两个本机编辑器和一个矢量转换组件 | 覆盖更多办公应用和服务 | 集成文档、表格、演示与 PDF 工具 |
| 交付方式 | 可构建的 Rust/JavaScript 源码、本机服务及 CLI | 官方应用与服务 | 官方应用下载及在线产品 |
| 云端协作 | 公开本机版不包含 | 通过 Microsoft 365 服务访问和协作文档 | WPS 生态提供云文档协作 |
| 源码与修改 | 按仓库许可提供源码；超出标准许可允许范围的商业用途须另行申请付费书面授权 | 按 Microsoft 产品条款使用 | 按 WPS 产品条款使用 |
| 本版扩展接口 | UniPPT/UniCell 本机 MCP；vecmeta Rust API 与 CLI | 本次资料核对未覆盖 | 本次资料核对未覆盖 |
| 性能证据 | 各仓库提供可复现的本机测评和原始记录 | 本轮未实测 | 本轮未实测 |

资料来源：[Microsoft 应用与服务](https://www.microsoft.com/en-us/microsoft-365/products-apps-services)、[Microsoft 协作文档](https://support.microsoft.com/en-us/office/collab-files/collaborate-from-anywhere-using-microsoft-365)、[WPS Office 产品页](https://www.wps.com/office/)、[WPS 公司与协作功能说明](https://www.wps.com/blog/what-is-kingsoft/)。


## 公开办公项目参照

2026-09-18 核对官方仓库：

| 项目 | 主要交付形态 | 与本项目的关系 |
| --- | --- | --- |
| [ONLYOFFICE Docs](https://github.com/ONLYOFFICE/DocumentServer) | 可部署的在线办公编辑器，包含实时协作能力 | 办公编辑器与文档集成的参照项目 |
| [Univer](https://github.com/dream-num/univer) | 以插件、渲染和公式引擎组成的可嵌入办公 SDK | 开发接入与模块化文档能力的参照项目 |
| OmniDoc | UniPPT / UniCell 本机应用与 vecmeta Rust 组件 | 当前公开源码围绕本机编辑、格式处理与 AI / MCP 工作流 |

三者交付形态与许可不同；这张表说明项目定位，不推导功能完整性或速度排名。ONLYOFFICE、Univer 的开源与商业版本范围以其官方文档为准。

## 测评结论的适用范围

已公布的数据反映本次发行程序在一台机器上的运行表现，不能据此推导相对 Microsoft 365 或 WPS 的速度优势。文件生成、公式计算、渲染和交互编辑是不同操作，应分别测量。vecmeta 的 CLI 耗时不能直接与办公应用的文档打开耗时比较。

开展同机对测时，应采用相同且有权使用的文件，记录应用版本、版本类型、架构、计算设置、字体、文件哈希及冷/热状态。分别测量打开至计算完成、编辑至重算完成、保存至文件写入完成；先核对数值、对象结构与渲染结果，再比较延迟。公开失败项、全部观测值、中位数和 P95；启动耗时与依赖网络的 AI/云服务单独记录。只有双方完成同一协议下的测量后，才发布速度比值。

## 许可表述

当前 OmniDoc 自有代码采用 PolyForm Noncommercial 1.0.0，属于**源码可见许可，不是 OSI 批准的开源许可**。第三方组件保留各自许可证。源码公开及项目愿景的表述不改变许可条件。

Microsoft 365、Office、PowerPoint、Excel 和 WPS 的名称及商标属于相应权利人。本对照由独立项目提供。
