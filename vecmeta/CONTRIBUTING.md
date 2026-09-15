# Contributing / 贡献

欢迎提交可复现问题、最小 SVG/EMF 样例与测试。仅提交有权公开的材料；不要上传客户文件、密钥、商业合同或来源不明的样本。

修改后运行 `cargo test --workspace --locked`，并用 `cargo fmt --all -- --check` 检查格式。格式往返应分别验证原生语义、可见几何及源字节恢复，不将嵌入源数据的恢复结果当作任意目标软件渲染一致。

自有代码采用 [非商业源码许可](LICENSE)。贡献者保留自己的版权；提交 PR 不自动转让版权或授予商业再许可。需要商业再许可的贡献，维护者须取得单独明确授权。第三方贡献的许可边界必须记录。

Bug reports should include minimal, legally shareable input and reproduction steps. Do not upload customer files, credentials, contracts, or assets of unknown provenance. Run `cargo test --workspace --locked` and `cargo fmt --all -- --check`. Distinguish geometry, native semantics, and embedded-source byte recovery.

Contributors retain their copyright. A PR does not automatically transfer copyright or grant commercial sublicensing; obtain explicit additional permission where needed. Preserve third-party attribution and license boundaries.
