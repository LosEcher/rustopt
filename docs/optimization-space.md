# 方案空间：概念解释 / 结构化 / 数学 / 物理 / 跨语言

配套：[bottleneck-verified.md](bottleneck-verified.md)（瓶颈实测与已落地改动）、[compile-speed-disk-cache.md](compile-speed-disk-cache.md)、[low-level-and-cross-language.md](low-level-and-cross-language.md)

本文回答四件事：① 两个被压缩的说法到底指什么；② 什么是"结构级去依赖"；③ 还有哪些工程化/配置化、数学、物理方案；④ 引入 C / C# / Go / WASM 做辅助模块值不值。
**【实测】**＝本机本次测出来的；**【调研】**＝外部资料；**【假设】**＝有理由但未实测。

---

## 1. 两个说法的准确含义

### 1.1 "锁 nightly" 到底贵在哪

`-Zthreads=8` 是 `-Z` 前缀 = **不稳定开关**，只有 nightly rustc 接受。要用它，你就必须在使用它的地方钉住一个 nightly：

```toml
# 方案 A：整仓钉（连 release 也变）
# rust-toolchain.toml
[toolchain]
channel = "nightly-2026-10-06"     # 钉日期才稳定；写 "nightly" 会天天漂
```
```toml
# 方案 B：只让 dev 用（CI/release 保持 stable）
# .cargo/config.toml
[env]
RUSTFLAGS = "-Zthreads=8"           # 仍需用 `cargo +nightly` 调用
```

代价有四面，逐条都是真实的：

| 代价 | 说明 | 证据 |
|---|---|---|
| **缓存全废** | 编译器版本进 unit hash ⇒ 切一次工具链 = 该项目 unit 图**全量重编** | **【实测】**同一 warm target dir：stable→nightly 重编 **12/12** 个 unit；nightly→stable 也是 12/12（第一次切换各付一次全量） |
| **产物并存、磁盘翻倍** | 不同工具链的产物按不同 hash 同时留在 `target/`，cargo 不会回收 | **【实测】**verify-gate 的 target 一度 782 M（多套 profile/工具链并存）；切 profile 时 782 M → 990 M |
| **nightly 会漂移** | 写 `channel = "nightly"` 时每次 `rustup update` 换编译器 ⇒ 又一次全量重编；钉日期则停止获得修复，且与 CI 逐渐不一致 | 【调研】 |
| **本地/CI 分裂** | 你的 7 个仓 CI 都钉 stable/1.9x（rustopt 还钉 1.97.0 + MSRV 1.87）；开发用 nightly ⇒ 本地绿/CI 红这类问题；**release 仍只能用 stable** | 各仓 `Cargo.toml` / CI 配置 |

> 一句话：**"锁 nightly" = 用一个每夜变化、不受支持、且与你的发布链路不一致的编译器，换 dev 构建 12.8% 的延迟**。缓解：钉日期版、只在 dev/本地用（CI 保持 stable），或等 `--jobs-frontend` 稳定。

### 1.2 "CPU +17%" 是什么意思，什么时候反而有害

**【实测】**nightly 基线 CPU 43.12 s → 加 `-Zthreads=8` 后 **50.45 s（+17%）**，而墙钟 8.06 s → **7.21 s（−10.6%）**。

把前端（typeck / borrowck / MIR）从单线程改成多线程，**同一份有用工作要多烧一点 CPU 才能排得更紧**：线程协调与锁竞争、部分 query 被提前或重复计算、内存分配与带宽压力上升。这是延迟 ↔ 吞吐/能耗的经典交换。

什么时候**反而有害**：

* **机器已被别的构建/agent 占满**（你的环境很常见）：多 17% CPU 会去抢别人的核，**整体**墙钟可能变差；
* **笔记本在电池上或散热受限**：额外功耗触发降频，长构建里收益递减；**【假设】**未实测；
* **CI runner 核少（2–4 核）**：并行前端收益下降、峰值内存上升，可能 OOM；
* 判据：**只有当"平均并发 < 核数"时才值得**——本机实测平均并发 5.56/10（有闲置核），所以这里 −12.8% 成立。

---

## 2. 什么是"结构级去依赖"

定义：**不动编译器参数，改依赖图的形状**。因为 `T_wall` 由最长路径 `L` 决定，路径上少一环就少一段墙钟；工作总量 `W` 由 unit 数决定，少一个 unit 就少一份编译。

| # | 手法 | 本机证据 |
|---|---|---|
| 1 | **砍掉深链的一端** | verify-gate 的 `ureq→url→idna→icu_*→rustls→ring` 链值 **25% 墙钟**（反事实 8.01 s → 6.00 s）。不需要 TLS/HTTP 就用 `default-features = false` 或换轻量实现 |
| 2 | **重依赖变可选 feature** | `[features] http = ["dep:ureq"]`、`default = []` ⇒ 普通构建根本不编译它 |
| 3 | **dev 依赖隔离** | `criterion→ciborium→zerocopy-derive→syn 2.x` 只在 `cargo test/bench` 出现（unirun/session-index **实测** `cargo tree -e normal` 里没有）⇒ `cargo build` 不付钱。反之，把只有测试用的重库放进 `[dependencies]` 就是每次构建都付 |
| 4 | **去掉 proc-macro 账** | `syn` 是本机最大单体开销：verify-gate 冷编里 `syn 2.0.119` 2.45 s + `syn 3.0.6` 1.94 s ≈ **4.4 s**。每多一个 derive 宏依赖，就多一套 `proc-macro2/quote/syn/unicode-ident` 的 host 编译；手写 `impl` 或换无 derive 的库能直接删掉这条链 |
| 5 | **图要宽不要深** | 并发实测：t=0 有 34 个 unit 可跑，t≥6 s 只剩 **1–3** ⇒ 深链的尾巴决定了墙钟下界 |
| 6 | **自身 crate 切分 + build.rs** | 把"天天改"的代码留在最小叶子 crate；`build.rs` 昂贵且门控下游：verify-gate 里 `ring`（C/汇编 build script）1.96 s、`cc` 1.28 s，且 `ring` 卡在 `rustls` 前面 |

**判据工具**：`cargo tree -e normal`（区分构建 vs dev）、`cargo tree -d`（重复版本）、`cargo build --timings`（算最长路径）、`cargo-udeps` / `cargo-machete`（找没用到的依赖）、`cargo-llvm-lines`（找单态化爆炸）。
**可做指标**：最长路径长度、尾部 unit 数、`syn` 类 host 编译单元数、版本分支数（本机全机 1,484 条，dsfolder 7 仓已 14 → 0）。

---

## 3. 工程化 / 配置化方案（六层清单）

**A. 依赖图**
1. workspace 化 + 统一 lockfile（**实测限制**：成员 `[profile.*]` 会被忽略并告警；`session-index` 的 `release` 是 `opt-level="z"`，与另 6 个冲突 ⇒ 只在 profile 政策一致的子集合并）；
2. `[workspace.dependencies]` + `[patch]` 统一版本；
3. `cargo-hakari` 钉死 feature 集合（消除"构建范围不同导致 feature 变化 ⇒ 随机重编"）；
4. 定期 `cargo tree -d`，把"版本分支数"当指标。

**B. 缓存**
5. sccache 已接线（`~/.cargo/config.toml`）；**下一步是多级 + 远程**：sccache 0.17 的二进制里确认存在 `SCCACHE_MULTILEVEL_CHAIN`、`SCCACHE_WEBDAV_ENDPOINT/USERNAME/PASSWORD`、`SCCACHE_REDIS_*`、`SCCACHE_GHA_*`、`SCCACHE_S3` 等 ⇒ **本地磁盘一级 + WebDAV/S3 二级**，让 **M1 与 M3 互相复用依赖产物**（正好对应你们"开发在 M1、构建在 M3"的工作流）。风险：WebDAV 小对象延迟高、并发写语义弱；建议先用 NAS 上的 S3/MinIO 或只读二级。**【假设】收益 = 跨机重复劳动的消除，未实测**
6. CI 侧 `sccache-action`（GH Actions 工作区路径跨运行固定 ⇒ 命中率才成立）。

**C. 构建系统 / 工具链配置**
7. profile 分层：`dev`（`debug=0`、`incremental=false`，最快最省）／`dev-opt`（`[profile.dev.package."*"] opt-level=1`，运行时快的依赖）／`release`（默认）／`dist`（体积）；
8. **rust-analyzer 独立 target dir**（避免 RA 的 check 产物与你的 build 互相驱逐）——但要与 sccache 折中：设了 `CARGO_TARGET_DIR` 就丢跨项目复用，所以给 **RA 单独设**、命令行保持默认；
9. `cargo check` 做内循环（2–3×），只在需要跑的时候 `build`；
10. `cargo-nextest`（只省**运行**时间）；
11. 把 `target/` 移出云同步树（本机实测 `target/` 已被坚果云忽略，但是靠客户端的忽略规则）；
12. cargo `build-dir` 新布局：`target/`（最终产物）与 `build/`（中间产物）分家 ⇒ **清理粒度终于能按目录划**（注意现有清理工具都按旧布局找路径）。

**D. CI 工程化**
13. `Swatinem/rust-cache`（已有）+ `sccache-action`；`cargo-chef` / BuildKit `--mount=type=cache` 做容器层缓存；
14. 矩阵瘦身 + 固定 toolchain + `--locked` + 先 `cargo check` 全图（fail-fast 顺序）；
15. 把 target triple 放进缓存 key（rustopt 的 CI 已经这么做了）。

**E. 度量 / SLO**
16. 把四个数做成可追踪指标：**冷编墙钟、内循环墙钟、target du、缓存命中率**（本次基线：verify-gate 7.38 s / 0.84 s / 225 M / Rust 命中 50%）；
17. `cargo build --timings` 产物进 CI 归档；`-Zself-profile` 看 query 级缓存命中；
18. 把"版本分支数"和"最长路径"当回归指标。

**F. 产物生命周期**
19. du 口径清理、`incremental = false`、细档工具；`-Zgc` 目前不可用（已实测）。

---

## 4. 数学方案（都不是比喻，是可算的判据）

| 模型 | 式子 | 本机代入 | 结论 |
|---|---|---|---|
| **DAG 调度下界** | `T ≥ max(L, W/m)` | `L≈8.0 s`，`W/m = 44.5/10 = 4.45 s` | **卡在 L**：加核/并行无用，只能缩短 L 或减少 W |
| **Amdahl** | 串行段占比决定并行上限 | 串行尾巴 2.01/8.01 = **25%** | 并行部分加速到无穷的极限 = 剩下的串行段；**实测反事实 6.00 s 与预测吻合** |
| **缓存期望收益** | `E[T] = h·T_hit + (1−h)·T_miss` | `h = 9/13 ≈ 0.69`，`T_hit ≈ 0.7·T_miss` | 期望收益约 20%；**但真实上界是"可缓存部分在关键路径上的占比"**（见 §6 的修正） |
| **内容寻址的等价类** | 命中率 `= P(H(输入) 相同)` | key 含版本/绝对路径/`CARGO_*`/feature | 提升命中率 = **减少 key 输入集合的自由度**；"对齐版本 / 不设 `CARGO_TARGET_DIR` / 钉 feature"是同一件事的三种表现 |
| **集合覆盖上界** | 跨项目复用上界 = 依赖集合交集 | 7 仓可对齐分支 14→0；跨族群 13 条不可合并 | 复用上界明确，剩下的只能靠升级上游 |
| **排队论** | 就绪队列长度 vs 服务台数 | t=0 就绪 34 个、平均并发 5.56 | 系统处于**欠载区**（ρ<1）：加"服务台"无用，应减少作业量或缩短链 |
| **信息论/影响半径** | 增量 = 只重算"输入哈希变化"的下游闭包 | `touch main.rs` 只重编 1 个 crate | 把高频改动放叶子、把稳定代码抽出去 = 缩小这个闭包；`-Zincremental-info` 可观测 |

**可操作的数学结论**：先算 `L` 与 `W/m`，判断你是"路径受限"还是"吞吐受限"；路径受限就做 §2，吞吐受限才谈并行/加核。verify-gate 是**路径受限**（8.0 vs 4.45）。

---

## 5. 物理方案

| 方向 | 内容 | 本机实测 |
|---|---|---|
| **核拓扑（P 核 vs E 核）** | M1 Pro = 8P+2E。假设：把 rustc 铺到慢的 E 核会拖长尾巴，限制到 P 核可能更优 | ❌ **假设不成立**：`-j 8`（只用 P 核）墙钟 7.42 s、`-j 10` 7.38 s、`-j 12`（超订）**7.15 s**；三者 CPU 都是 ~39.2 s ⇒ 差异是调度噪声，**job 数不是杠杆**（与 §4 排队论一致） |
| **热/功耗墙** | 持续满载降频；`-Zthreads` 多烧 CPU ⇒ 长构建收益递减 | **【假设】**未实测；`pmset -g therm` 可看降频状态 |
| **内存带宽/缓存层级** | 前端是内存密集（AST/HIR），并行前端加大带宽压力（也解释 CPU +17%）；M1 统一内存是共享资源 | 未直接测；CPU↑ 而墙钟↓ 的形态与之相符 |
| **IO / 文件系统** | APFS 元数据开销、海量小文件（unirun 的 `.o` 13,866 个）、cargo 的硬链接复用；`-Zchecksum-freshness` 把 stat 换成读校验和 | 新鲜度检查 **0.07 s**（非瓶颈）；`.o` 硬链接 6,856 inode / 516 MB |
| **调度/QoS** | macOS 上给构建进程更高 QoS、避免同时跑多个构建互相抢核 | 未测 |
| **内存容量** | 并行前端峰值内存更高；swap 会毁掉墙钟 | 未测 |

---

## 6. 跨语言：C / C# / Go / WASM 能不能当"编译加速器"

判据先立：**只有当"省下的编译时间 > 引入的复杂度 + 新依赖自身的编译时间"时才成立**。

| 方案 | 能省编译时间？ | 适用条件 | 结论 |
|---|---|---|---|
| **Go：把构建期的重活搬出 Rust 编译图** | ✅ 有条件成立 | `build.rs` 里做代码生成/模式解析/资源打包时，那段 Rust 自己要编进 **host 图**且每次失效都重跑。换成**预编译的 Go 单二进制**，`build.rs` 退化成 `Command::new("gen")` | **唯一值得认真评估的**。Go 单文件、秒起、`GOOS/GOARCH` 交叉编译容易。代价：多一个工具链/二进制要在所有机器与 CI 存在、`rerun-if-changed` 要覆盖它、供应链多一环 |
| **Go：常驻守护进程** | ⚠️ 收益有限 | 类比 Roslyn 的 `VBCSCompiler`（常驻、复用已加载引用）；cargo 的固定成本本来只有 **0.07 s**（实测） | 对"每次重新分析依赖图"这类固定成本有意思；对 cargo 本身没必要 |
| **C：替换重型泛型/宏模块** | ⚠️ 仅当替换真正的编译热点 | C 没有单态化/trait 解析/borrowck，确实能砍编译时间；但代价是 FFI unsafe + 平台工具链耦合，而 `cc`/`bindgen` **自身就是编译期成本**（`cc` 实测 1.28 s） | **你已经在付 C 的账**：verify-gate 的 `ring`（C/汇编）1.96 s 且卡在关键路径。引入更多 C 通常**增加**复杂度。要用 `cargo --timings` 先证明热点 |
| **WASM：预编译子系统，宿主解释执行** | ❌ 对构建加速基本为负 | 想法是"编译时间→0、运行期用 wasmi/wasmtime"；但引入 wasm 运行时本身是巨大依赖树（往往比被替换的子系统更重），运行期性能下降，wit-bindgen 等还要额外代码生成 | **适合发布形态/插件，不适合构建加速** |
| **C#** | ❌ | 可借鉴的只有"编译器服务器"与"up-to-date 检查"两个**思想**，而 Rust 侧已有对应物（rust-analyzer、cargo fingerprint） | 引入 .NET 运行时（启动慢、体积大）在 macOS/Linux CI 上不划算，**不推荐** |
| **真正值得做的"跨语言"是"跨机器"** | ✅✅ 零代码 | `SCCACHE_MULTILEVEL_CHAIN` + WebDAV/S3/Redis/GHA 后端（0.17 二进制里确认存在这些变量） | 把 NAS/对象存储当二级缓存，让 **M1 与 M3 互相复用依赖产物**——比任何"换语言重写模块"的收益都大 |

---

## 7. 重要修正：sccache 的收益是"命中率的函数"，不是固定倍数

我先后测了三种缓存状态，结论必须一起看（verify-gate，86 crate，同一配置 `[profile.dev.package."*"] debug = 0`）：

| 条件 | 墙钟 | CPU(user+sys) | sccache |
|---|---|---|---|
| **keys 全匹配（缓存全热）** | **3.29 s** | 9.4 s | **106 命中 / 0 未命中** |
| 全冷（无 wrapper） | **6.97 s** | 38.61 s | — |
| 部分匹配（缓存未覆盖全部 key） | 8.27 s | 9.48 s | 总命中率 61.3%（Rust 50% / C/C++ **100%** / Assembler **100%**） |

以及小仓（rustopt，12 unit）：无 wrapper 4.44 s → 全 miss 5.03 s → 全 hit **3.09 s**。

**规律**：`命中率 = P(H(源内容, 编译参数, 依赖绝对路径, 环境) 相同)`。

* **keys 匹配**：−53% 墙钟、−76% CPU（本机最大一块稳定收益之一）；
* **keys 不匹配**：0 命中，墙钟不改善甚至略慢（未命中路径要多付 +13% 的 wrapper/存取开销）。

即便整体命中率很高，墙钟仍受**不可缓存单元**限制（`--crate-type bin` 一律不缓存、build script 执行、链接都要钱）。所以 tail-bound 项目的墙钟收益上界 = **可缓存部分在关键路径上的占比**（§4 的 Amdahl：25% 串行 ⇒ 极限 6.00 s）。

**正确的结论（三条口径分开说）**：

1. **CPU / 能耗 / CI 成本**：明确变好（−76% CPU）；这是"稳定"的那一半。
2. **墙钟**：**= f(命中率)**。keys 匹配时 −53%；不匹配时 0 或略负。**要让命中率稳定，就必须管住 key 的四类自由度**（版本、绝对路径、feature、环境）——这就是"版本对齐 + 不设 `CARGO_TARGET_DIR`"的真正价值。
3. **跨项目 / 跨机器**：真实有效（fmtguard → run-diff 冷编 9 命中、2.73 s；CI 路径固定时才是主场）。

⇒ 接线保持（零指纹成本、CPU 大降、跨项目有效），但**不要承诺固定倍数**；墙钟要靠 §2 缩短关键路径 + `debug=0` +（可选）关键单元的 `-Zthreads`，并**把命中率本身做成常规指标**（见设计文档 §4）。
