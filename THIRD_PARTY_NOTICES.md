# 第三方组件 / Third-party notices

| Component | Version / origin | License / location |
| --- | --- | --- |
| IronCalc | 0.8.2, [upstream](https://github.com/ironcalc/IronCalc) | MIT OR Apache-2.0; resolved through Cargo |
| Patched ironcalc_base | 0.8.2, [source and change notice](server/vendor/ironcalc_base/UNICELL-NOTICE.md) | [MIT](server/vendor/ironcalc_base/LICENSE-MIT) OR [Apache-2.0](server/vendor/ironcalc_base/LICENSE-Apache-2.0) |
| KaTeX | 0.16.9, official npm distribution | [MIT](web/vendor/katex/LICENSE); served locally |
| vecmeta | included public source | [LICENSE](vecmeta/LICENSE), [NOTICE](vecmeta/LICENSE-NOTICE.md) |
| latex2mathml | optional Python dependency | MIT, installed separately for math export |
| mathml2omml | optional Python dependency | LGPL-3.0, installed separately; [upstream](https://github.com/amedama41/mathml2omml) |
| lxml | optional Python dependency | BSD and bundled-library terms; installed separately |
| Other Rust dependencies | exact versions in server/Cargo.lock and vecmeta/Cargo.lock | Their own crate licenses, not overridden by the application license |

No commercial font collection is distributed. Optional user fonts retain their own terms. Dependency libraries are independently authored and are not claimed as OmniDoc inventions. The application project's domestic origin does not replace these notices.
