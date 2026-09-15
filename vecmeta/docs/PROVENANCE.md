# 来源与发行边界 / Provenance and distribution

## 原创代码

项目维护者于 2026-09-15 明确确认 vecmeta 为 OmniDoc 原创项目并具有重新许可权。当前自有源码采用根目录 [LICENSE](../LICENSE)。历史 `Cargo.toml` 中的 GPL-2.0 和 `kakwa/libemf2svg` repository 字段不再用作当前发布元数据；旧版本已经依法授予的权利不追溯撤销。

公开仓库包含六个 Rust crate、CLI、自包含测试、中英文说明、许可文本和验证配置。EMF/SVG 转换代码不通过 C 转换库或 Office 自动化运行。Cargo 第三方依赖继续遵循各自协议。

## 测试材料

本地历史 `tests/resources/` 样本、`test1.emf` 与 `test1.svg` 不进入公开源码快照；样本来源和授权独立于 Rust 原创代码。公开发布脚本采用显式目录清单，不复制原 `.git`、历史外部样本或本机配置。依赖外部样本的四项测试显式 ignored，运行时要求设置 `VECMETA_CORPUS_DIR`，不会因找不到材料而静默通过。

本地已有的非致命兼容性报告接口随当前源码快照发布；准备过程不覆盖原目录的已有实现修改。公开快照单独接受构建与测试。

## English

On 2026-09-15 the maintainer confirmed OmniDoc's independent authorship and relicensing authority for vecmeta. Current first-party source uses [LICENSE](../LICENSE). The historic GPL-2.0 and `kakwa/libemf2svg` repository fields are not the current release metadata. Earlier lawful grants remain valid.

The public distribution contains six Rust crates, the CLI, self-contained tests, bilingual documentation, licensing, and verification configuration. Conversion does not invoke C conversion libraries or Office automation. Cargo dependencies retain their own licenses.

Historical external `tests/resources/` assets and local `test1.emf` / `test1.svg` are excluded. Their provenance is separate from first-party code. An allowlisted snapshot excludes the original `.git`, external corpus, and machine-local configuration. Four corpus tests are explicitly ignored and require `VECMETA_CORPUS_DIR`; absent samples must not silently pass.

Existing non-fatal compatibility-report APIs are included in the current source snapshot without overwriting their local implementation changes. The exported snapshot is built and tested independently.
