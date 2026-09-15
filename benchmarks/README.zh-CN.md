# UniCell — SUM 优化与性能测评

[English](README.md) | **简体中文**

有界 SUM 范围优化已同步至公开本机版。同一台机器、相同 1 万行 CSV（10 万单元格、2 万公式），导入并计算中位数从 **20.217 秒降至 0.661 秒，约快 30.6 倍**。每项预热 2 次、正式测量 7 次。

这是单机合成工作负载的前后观测，不是 Excel、Office 365 或 WPS 的速度对比，也不代表全部表格功能的整体提速。共享工作站的后台负载与温度未完全受控。XLSX 导出等未针对优化的路径可能变慢，所有结果均公开保留。公网服务部署状态不由本仓库更新改变。

## 优化机制

旧实现在计算每个 `SUM(A行:H行)` 时扫描整张工作表以确定已用范围，使大量独立行求和产生重复工作。新版仅在整行或整列引用时查询已用范围；有明确边界的引用直接遍历该范围。公式计算仍然完整执行，没有用缓存结果替代计算。补丁及回归测试位于 `server/vendor/ironcalc_base/`，该第三方组件及补丁继续采用 MIT OR Apache-2.0。

## 环境

| 字段 | 记录 |
| --- | --- |
| OS | Microsoft Windows 10 专业版 (10.0.19045, AMD64) |
| CPU | 11th Gen Intel(R) Core(TM) i7-1165G7 @ 2.80GHz (4 cores / 8 threads) |
| RAM | 31.70 GiB |
| Rust | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| Python | 3.13.11 |
| 新版构建源提交 | `dc6f6c6a5c5de532e93b3adbf6a00336562a1a9f` |
| 旧版构建源提交 | `51e007388d702f23290bb44df7270d164daa0e2d` |
| Profile | `cargo build --release --locked --manifest-path server/Cargo.toml; default opt-level=3; ironcalc and ironcalc_base opt-level=1` |
| Binary SHA-256 | `eb228b490c20361927a2bea290de3a36752896181192b773241ef1710f2c884e` |
| Harness SHA-256 | `769a6efb2d6b90b95dd5d04dda8a9aba46fec4ae2e80226f1d4add9e185b78e8` |
| UTC | 2026-09-15T23:32:43.714362+00:00 |


## 工作负载与正确性

单工作表、十列：八列数值输入、一列 `SUM(A:H)`、一列依赖乘法。CSV/XLSX 导入包含自动计算；批量编辑修改每行 A 列并包含重算和历史记录。新版每次预热和正式运行均验证 XLSX 的 ZIP 完整性、全部单元格及公式数量，并逐一核对每条公式的表达式和缓存数值；1 万行对应全部 **2 万公式**。导入和编辑完成后的验证导出、解析与断言均在计时之外。历史基线只抽查首、中、末行数值，因此验证流程强度不同，运行间导出也会改变缓存和负载状态。

## 公开版优化后结果

| 操作 | 规模（行） | 中位数 ms | P95 ms |
| --- | ---: | ---: | ---: |
| CSV 导入与计算 | 100 | 10.25 | 21.82 |
| XLSX 导出 | 100 | 10.93 | 36.19 |
| XLSX 导入与计算 | 100 | 14.21 | 17.15 |
| 批量编辑与重算 | 100 | 2.59 | 3.14 |
| CSV 导入与计算 | 1,000 | 68.16 | 79.31 |
| XLSX 导出 | 1,000 | 48.55 | 57.71 |
| XLSX 导入与计算 | 1,000 | 80.59 | 96.26 |
| 批量编辑与重算 | 1,000 | 13.00 | 15.96 |
| CSV 导入与计算 | 10,000 | 660.92 | 707.79 |
| XLSX 导出 | 10,000 | 562.30 | 618.65 |
| XLSX 导入与计算 | 10,000 | 1144.12 | 1248.97 |
| 批量编辑与重算 | 10,000 | 120.68 | 135.50 |

[JSON](results/2026-09-16-sum-optimized-windows-x64.json)

## 保留的优化前基线

| 操作 | 规模（行） | 中位数 ms | P95 ms |
| --- | ---: | ---: | ---: |
| CSV 导入与计算 | 100 | 4.53 | 19.19 |
| XLSX 导出 | 100 | 27.01 | 29.34 |
| XLSX 导入与计算 | 100 | 11.85 | 21.56 |
| 批量编辑与重算 | 100 | 16.42 | 16.76 |
| CSV 导入与计算 | 1,000 | 71.07 | 79.10 |
| XLSX 导出 | 1,000 | 41.61 | 48.33 |
| XLSX 导入与计算 | 1,000 | 82.79 | 86.61 |
| 批量编辑与重算 | 1,000 | 33.82 | 48.10 |
| CSV 导入与计算 | 10,000 | 20217.27 | 28130.84 |
| XLSX 导出 | 10,000 | 452.73 | 557.95 |
| XLSX 导入与计算 | 10,000 | 17865.22 | 24569.10 |
| 批量编辑与重算 | 10,000 | 19081.38 | 23292.38 |

[JSON](results/2026-09-16-windows-x64.json)

## 早期约 55 倍实验

此前完整版服务的优化实验记录为 **20.217 秒 → 0.368 秒，约 55 倍**，每轮同样验证 2 万公式及结果。该实验的优化后程序与本公开本机版不是同一构建，服务入口和测评流程也不同；这组数值保留为原始优化实验，不能替代上方公开版复测。实验 JSON 仅将本机绝对程序路径改为文件名，时延、校验字段和二进制哈希保持原值，并记录原文件哈希。

[JSON](experiments/2026-09-16-csv-sum-fixed.json)

## 方法与限制

HTTP 计时从发送本机请求到完整读取响应，包含处理及计算，不含输入生成、验证、应用启动或浏览器渲染。所有正式样本均保留；中位数为排序后第 4 项，P95 采用最近秩法，7 次测量下等于最大值，不是稳定尾延迟估计。测试未涵盖复杂真实工作簿、并发、峰值内存、UI 帧率、AI 或云端服务。旧 JSON 与旧版完整表格保留；历史测评脚本见 [基线发布](https://github.com/OmniDocX/unicell/tree/44bfc75e3c49eda4ac408f50c711ce70b0e34e7a/benchmarks)。

## 复现与回归测试

本机 release 回归验证通过：13 项 SUM 测试、4 项三维引用测试，均无失败。CI 也要求运行这两组测试；此处不表示完整计算引擎测试集全部通过。

```sh
cargo build --release --locked --manifest-path server/Cargo.toml
python benchmarks/run.py --binary server/target/release/opencell-server --sizes 100,1000,10000 --warmups 2 --samples 7 --output benchmarks/results/local.json
cargo test --locked --manifest-path server/vendor/ironcalc_base/Cargo.toml --lib test_fn_sum::
cargo test --locked --manifest-path server/vendor/ironcalc_base/Cargo.toml --lib test_fn_3d_references::
python benchmarks/verify_results.py
```
Windows 程序路径追加 `.exe`；脚本只使用 Python 标准库，启动并关闭自有回环服务。
