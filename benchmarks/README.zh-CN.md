# UniCell — 性能测评

[English](README.md) | **简体中文**

2026-09-16 在 Windows 10 / Intel Core i7-1165G7 / 31.70 GiB 内存上实测。每项预热 2 次、测量 7 次，顺序执行。

## 测试环境

| 字段 | 记录 |
| --- | --- |
| OS | Microsoft Windows 10 专业版 (10.0.19045, AMD64) |
| CPU | 11th Gen Intel(R) Core(TM) i7-1165G7 @ 2.80GHz (4 cores / 8 threads) |
| RAM | 31.70 GiB |
| Rust | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| Python | 3.13.11 |
| 构建源提交 | `51e007388d702f23290bb44df7270d164daa0e2d` |
| 构建配置 | `cargo build --release --locked --manifest-path server/Cargo.toml; default opt-level=3; ironcalc and ironcalc_base opt-level=1` |
| 程序大小 | 22,524,416 bytes |
| 测量时间 UTC | 2026-09-15T23:02:50.938714+00:00 |

## 工作负载与计时边界

单工作表、十列：八列数值输入、一列 SUM(A:H)、一列依赖乘法。导入包含自动计算；批量编辑修改所有行的 A 列，包含自动重算及历史记录开销。XLSX 输出核对全部单元格与公式数量，API 检查首行、中间行、末行的依赖结果与公式保留。这属于抽样数值检查，并非穷尽式工作簿兼容性测试。

HTTP 计时包含请求传输、服务处理和完整响应读取；输入生成与结果检查在计时区间外。服务在测量前已启动，不包含浏览器渲染。

## 完整结果

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

[原始数据](results/2026-09-16-windows-x64.json) · [测评脚本](run.py)

## 统计方法与限制

预热样本不计入统计。中位数为第 4 个排序样本；P95 使用最近秩法 `ceil(0.95 × n)`，在 n=7 时等于本次最大值，不能视为稳定尾延迟估计。原始 JSON 保留全部观测值、输入/输出大小或哈希、校验结果、二进制与脚本 SHA-256。此处的源提交是构建时运行时代码的版本；本次新增文档与脚本未修改运行时代码。

测试在共享工作站上进行，未控制所有后台负载、功耗或文件缓存状态；没有剔除慢样本，也没有执行并发压力测试。结果仅适用于这些合成工作负载；未测峰值内存、完整应用启动时间、浏览器帧率、真实复杂文档、外部 AI 和云服务。Microsoft 365（Office 365）与 WPS Office 本轮未实测，不能从这些数据推导相对速度排名。

## 复现

```sh
cargo build --release --locked --manifest-path server/Cargo.toml
python benchmarks/run.py --binary server/target/release/opencell-server --sizes 100,1000,10000 --warmups 2 --samples 7 --output benchmarks/results/local.json
python benchmarks/verify_results.py
```

Windows 下为程序路径追加 `.exe`；若使用 `--target-dir`，应相应修改 `--binary`。脚本只使用 Python 标准库，输入完全由脚本生成；服务型测评会启动和关闭自己的临时回环服务进程。使用相同源码、锁文件、编译器与构建配置复现；时间因机器与负载而异。

[Office 365 / WPS 对照与后续同机测试协议](../docs/COMPARISON.zh-CN.md)
