# docs 索引

本目录是"Rust 构建速度 / 磁盘占用 / 缓存复用"这轮调查的全部记录。**先读 FINDINGS，再读 DESIGN**；其余是过程与背景材料。

| 文档 | 内容 | 什么时候读 |
|---|---|---|
| **[FINDINGS.md](FINDINGS.md)** | **结论与台账**：判定表（方案 × 显著 × 稳定 × 成本）、被实测推翻的 9 条说法、全部实测数字基线、变更台账与回滚、未决事项、触发重估的条件 | **首先读这个**——它是对"有没有显著且稳定的方案"的直接回答 |
| **[DESIGN-build-cache-governance.md](DESIGN-build-cache-governance.md)** | **设计文档**：目标/非目标、6 条设计原则、五类策略（profile / 缓存 / 版本 / 清理 / 工具链）、SLO 与 CI 门禁、`rustopt` 四个子命令的实现设计、分阶段 rollout 与回滚、风险与上游触发条件 | 要落地或改流程时 |
| [bottleneck-verified.md](bottleneck-verified.md) | 机制链路（cargo 指纹 / mtime / unit hash）、verify-gate 的瓶颈实测（W/L/并发曲线/反例 25%）、5 臂冷编与 4 臂内循环对照、sccache 双向验证、**已落地改动与验证** | 想看"为什么"和原始对照 |
| [optimization-space.md](optimization-space.md) | 概念解释（锁 nightly / CPU +17%）、结构级去依赖的 6 种手法、六层工程化清单、数学方案（DAG 下界 / Amdahl / 期望收益 / 等价类 / 排队 / 信息论）、物理方案（核拓扑实测否定 / 热 / 带宽 / IO）、跨语言（C / C# / Go / WASM 的判据与结论） | 想扩展方案空间时 |
| [compile-speed-disk-cache.md](compile-speed-disk-cache.md) | 第一轮：编译加速与磁盘清理的选项清单、全机 14 个 target 与各类缓存盘点、sccache 首次实测、三个 `rustopt` 候选功能 | 背景 |
| [low-level-and-cross-language.md](low-level-and-cross-language.md) | 第一轮：本机 nightly 的 `-Z` 开关清单（实读）、unit hash 与版本分叉、系统层（硬链接 / clonefile / 同步目录 / 分布式）、可参考的开源实现、其他语言的对照 | 背景 |
| [evidence/](evidence/) | 原始执行日志（版本对齐、debug info + sccache 落地） | 核对数字 |
| [evidence/rollback/](evidence/rollback/) | **所有已改文件的改动前副本**（7 个 `Cargo.lock` + `verify-gate/Cargo.toml` + 已应用的 `~/.cargo/config.toml`） | 需要回滚时 |

## 三句话版本

1. **构建速度**：没有单一"显著且稳定"的开关。稳定档 = 依赖 debug info 剥离（磁盘 −40% / 墙钟 −12%）+ 缓存（CPU −76%；墙钟 = f(命中率)，keys 匹配时 −53%）+ 版本对齐（可对齐分叉 14→0）；显著档（`-Zthreads` −12.8%）要 nightly；最大的一块（verify-gate 的 25%）在依赖图里，是产品决策。
2. **磁盘**：能稳定清理（全机 **5.59 GB** 机械可回收；verify-gate **999 M → 233 M** 已兑现），但必须**清一次 + 改配置防再生**；跨项目那 ~4 GB 重复只能靠版本对齐，其中 13 条不兼容族群无法合并。
3. **天花板**：墙钟由关键路径决定（Amdahl）、复用粒度是 crate、没有内容寻址的全局缓存、单 unit 前端单线程——这四条不是本地配置能突破的；上游一旦落地 cross-workspace cache / 稳定并行前端 / build-dir 新布局，需按 FINDINGS §7 重跑判定表。
