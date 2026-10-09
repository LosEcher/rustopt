# 瓶颈实测与优化验证（第一性原理版）

配套：[compile-speed-disk-cache.md](compile-speed-disk-cache.md)（选项与清理）、[low-level-and-cross-language.md](low-level-and-cross-language.md)（更底层机制 / 开源实现 / 其他语言）

测量对象：`verify-gate`（161 个包 → 编译 86 个 crate → 112 个 unit；deps: regex/serde/serde_json/sha2/toml/ureq）
测量机：Apple M1 Pro，**10 逻辑核（8P+2E）**，cargo/rustc 1.97.0 stable + 1.100.0-nightly
方法：每臂**全新 `CARGO_TARGET_DIR` 串行冷编**（无并发干扰），墙钟由 `/usr/bin/time -p` 与 cargo 自报双向核对；结构数据来自 `cargo --timings` 的 `UNIT_DATA`。全部数字为本次实测，非估算。

---

## 1. 现在是怎么做的（机制链路，含实测证据）

```
cargo build
 ├─ 解析依赖图 → 生成 unit（每个 crate × profile × features × 目标种类）
 ├─ 对每个 unit 算 hash（包 id + features + profile + 【依赖的 hash 链】）→ 产物名带 -C extra-filename
 ├─ 新鲜度检查：比对 .fingerprint/<pkg>-<hash>/dep-<kind>-<name> 里记录的 mtime
 │    fresh → 跳过；dirty → 调 rustc
 ├─ rustc：前端（parse/expand/typeck/borrowck/MIR）【单线程】
 │         后端 codegen（dev 默认 codegen-units=256，可并行）
 └─ 最后一个 unit 内由 rustc 调 cc/Apple ld 完成链接（本机 Apple ld-prime）
```

**没有内容寻址的全局缓存**：产物只活在各自的 `target/` 里；`sccache` 是外挂的编译器级 CAS，而且 key 里混入了 `CARGO_*` 环境变量（§4 实测）。

实测证据（verify-gate，112 个 unit 的图）：

| 观察 | 结果 |
|---|---|
| warm no-op 构建成本 | **0.07 s**（real），日志无一条 dirty ⇒ **新鲜度检查不是瓶颈** |
| `touch src/main.rs` 后重编 | **只有 1 个 crate** 被重编（`verify-gate`），0.88 s |
| dirty 的判定原文 | `dirty: FsStatusOutdated(StaleItem(ChangedFile { reference: ".../dep-bin-verify-gate", ... }))`，附 mtime 比较 `FileTime{1791321017.77} < FileTime{1791321018.59}` |
| 推论 | 判据是 **mtime + dep-info**。项目在坚果云同步树内 ⇒ 同步/检出/备份工具碰一下 mtime 就会触发重编；`-Zchecksum-freshness` 正是上游对此的应对 |

---

## 2. 瓶颈在哪（verify-gate 冷编，stable dev）

| 指标 | 数值 | 读法 |
|---|---|---|
| 墙钟 | **8.27 s** | |
| CPU 时间 | 45.95 s（user 40.8 + sys 5.1） | 工作量 |
| **平均并发** | **5.56 / 10 核 = 56%** | 机器**没跑满** |
| 前端合计 vs 后端合计 | 26.9 s vs 8.7 s | **前端 76%**（且单 unit 内单线程） |
| 并发曲线 | t=0 有 **34** 个 unit 可跑；t≥6 s 掉到 **1–3** | 瓶颈是**尾巴（关键路径）**，不是 job 数 |
| 尾巴链 | `rustls` 4.98→7.21 s（2.23 s）→ `verify-gate` 7.22→8.01 s（0.79 s）；其前是 `icu_*`→`idna`→`url`→`ureq` | 深链串行 |
| **反事实** | 去掉 `ureq→rustls→icu→url→idna` 这条链，构建在 **6.00 s** 结束 | **该链值 2.01 s = 全墙钟的 25%** |
| 单项目内已存在的版本分叉 | `syn 2.0.119`（2.45 s）与 `syn 3.0.3`（1.94 s）**各编一遍** | 重复劳动在项目内部就发生 |
| 链接 | 含在最后那个 unit 的 0.79 s 里 | **不是瓶颈**（与"macOS 无链接器杠杆"一致） |

**结论**：这台机器上，慢的原因不是核不够、不是链接器、不是 IO，而是三件结构性的成本：
1. **重复劳动**（跨项目、跨版本、甚至项目内部）；
2. **依赖图深链造成的串行尾巴**（最后 2.5 s 只有 1–3 个 unit 在跑）；
3. **单 unit 前端单线程**（前端占全部工作量的 76%）。

---

## 3. 优化对照（冷编，每臂全新 target dir）

| 臂 | 墙钟 | CPU | target 体积 | 相对基线 |
|---|---|---|---|---|
| baseline（stable, dev） | 8.27 s | 45.95 s | 372 M | — |
| `debug=0`（stable） | 7.54 s | 39.19 s | **225 M** | 墙钟 **−8.8%**，CPU −15%，磁盘 **−40%** |
| nightly 基线（前端 1 线程） | 8.06 s | 43.12 s | 303 M | −2.5% |
| nightly `-Zthreads=8` | 7.21 s | 50.45 s | 303 M | 墙钟 **−12.8%**，CPU **+17%** |
| **nightly `-Zthreads=8` + `debug=0`** | **5.96 s** | 45.71 s | **158 M** | 墙钟 **−28%**，磁盘 **−58%** |

内循环（改一行后重编，同一批 warm target dir）：

| 臂 | 墙钟 | 相对基线 |
|---|---|---|
| baseline | 0.91 s | — |
| `debug=0` | 0.78 s | −14% |
| `-Zthreads=8` | 0.62 s | −32% |
| 组合 | **0.61 s** | **−33%** |

读法：
* `debug=0` 只值 8.8% 墙钟，但**磁盘值 40%**——与"codegen 只占工作量 24%"完全一致（它主要动后端与 IO）。
* `-Zthreads=8` 值 12.8% 墙钟，代价是 **CPU +17%**（用 CPU 换延迟）——与"前端占 76% 且单线程"一致。
* 两者叠加接近线性（−28%），说明它们打的是不同的部位。
* nightly 基线本身就比 stable 小一点、快一点（−2.5%，target 303 M vs 372 M），后者我**没有解释清楚**，仅作观察记录。

---

## 4. 缓存复用的双向验证（sccache 0.17.0，副本实验）

把 `rustopt` 与 `run-diff` 拷到 `/tmp` 各建一次冷编（`RUSTC_WRAPPER=sccache`，两项目都已 `incremental = false`）：

| 配置 | 命中 / 总 | 墙钟 |
|---|---|---|
| 版本**不一致** + 各自默认 `target/`（不设 `CARGO_TARGET_DIR`） | **5** / 17 | 3.99 s |
| 版本**对齐** + 各自默认 `target/` | **21** / 22 | **2.75 s（−31%）** |
| 版本对齐 + 再编一次（同默认 target dir） | **22** / 22 | 2.28 s |
| 版本对齐 + **显式** `CARGO_TARGET_DIR`（路径与默认完全相同） | 17 / 22 | 3.79 s |

* 对齐只改了**两个 crate**：`syn 3.0.3→3.0.6`、`unicode-ident 1.0.24→1.0.26`（`cargo update -p … --precise …`，**离线即可完成**）。
* **机制结论（双向证实）**：sccache 默认就能跨项目复用依赖（5 命中）；而只要 `CARGO_TARGET_DIR` 这个**环境变量存在**——哪怕值和默认路径一模一样——命中立刻掉回 0/5。
  ⇒ "给每个项目/每个变体设独立 target dir"这个常见做法，会**静默地把跨项目缓存复用清零**。`rustopt` 自己就是这么做的（每个变体一个 work dir），所以它的变体间永远不可能复用。
* 版本分叉是剩下的那一半：全机 24 个 `Cargo.lock`、**1,484 条冗余版本分支**（`syn` 12 个版本）。

---

## 5. 第一性原理：杠杆分级

成本模型：`T_wall = 关键路径`，`Work = Σ(unit 的前端 + 后端)`，`并发利用率 = Work / (T_wall × 核数)`。
本机实测：`Work = 44.5 s`，`T_wall = 8.27 s`，利用率 56%。

| 层级 | 杠杆 | 为什么它在这一级 | 实测/预期 |
|---|---|---|---|
| **1. 乘法级** | **消除重复劳动**：版本对齐、跨项目共享缓存、不设 `CARGO_TARGET_DIR` | 唯一能真正减小 `Work` 的杠杆；别的只是重新分配 | 复用 5→21 unit、墙钟 **−31%** |
| **2. 结构级** | **缩短关键路径**：去掉深依赖链、消除项目内的重复版本 | `T_wall` 由最长路径决定，不 concurrency | 那条 `ureq/rustls/icu` 链值 **25%** 墙钟；项目内两个 `syn` 各 ~2 s |
| **3. 并行级** | 单 unit 前端并行（`-Zthreads`） | 前端占 76% 且单线程 | **−12.8%** 墙钟，CPU +17% |
| **4. 单 unit 工作量** | debug info、`-Zcache-proc-macros`、hint | 只动后端/IO（24%） | `debug=0`：−8.8% 墙钟、**−40% 磁盘** |
| **无效项** | 加核、换链接器、优化 IO | 并发已 34 起跳（尾巴只有 1–3）；macOS 无链接器可换；新鲜度检查 0.07 s | 都不要投 |
| **6. 更深（超出 cargo 现状）** | 内容寻址的全局 action cache + 更细的接口/ABI | 才能把"重复劳动"逼近 0 | cargo cross-workspace cache 目标；Go/Nix/Bazel 已在做 |

---

## 6. 可执行清单（含回滚）

| 优先级 | 动作 | 实测收益 | 成本/风险 | 回滚 |
|---|---|---|---|---|
| **P0** | **对齐版本**：`cargo tree -d` 找分叉 → `cargo update -p <crate> --precise <ver>`；或把 profile 政策一致的仓并入一个 workspace | 复用 5→21 unit，墙钟 −31%；全机 1,484 条冗余分支是这 4.05 GB rlib 重复的根因 | 锁文件变更（正常依赖升级）；合并 workspace 时成员 `[profile.*]` 会被忽略 | `git checkout -- Cargo.lock` |
| **P0** | **需要 sccache 复用的场景不要设 `CARGO_TARGET_DIR`**；`rustopt` 若要缓存变体产物，需先测"同 target dir 放全部变体"是否更快 | 保住跨项目复用的那 5 个 unit（占本实验 29%） | 与"按项目隔离 target dir"的既有习惯冲突 | 去掉环境变量即可 |
| **P1** | `[profile.dev] debug = 0`（或 `"line-tables-only"`） | **磁盘 −40%**、墙钟 −8.8%（本机 372 M→225 M） | 断点/回溯质量下降；`line-tables-only` 可折中 | 删掉该行 |
| **P1** | 开发用 nightly + `-Zthreads=8`（写进 `.cargo/config.toml` 的 `[env] RUSTFLAGS`） | 墙钟 −12.8%、内循环 −32% | 锁 nightly；CPU +17% | 去掉 RUSTFLAGS |
| **P2** | 两者组合 | **墙钟 −28%、磁盘 −58%、内循环 −33%**（8.27→5.96 s；372→158 M；0.91→0.61 s） | 上面两条的代价叠加 | 同上 |
| **P2** | 结构性去依赖：`ureq` 的 TLS 链（`rustls`/`ring`/`icu`/`idna`/`url`）是 25% 墙钟的来源；换更轻的 HTTP/TLS 组合可缩短关键路径 | 预估 ≤25%（**未实测**，需改依赖与代码） | 功能/安全取舍，属产品决策 | 依赖回退 |

---

## 7. 对 rustopt 的含义

这轮验证把"该测什么"讲清楚了。`rustopt` 已经有 `duration_ms`，但只报一个总数会漏掉真正的瓶颈：

1. **同一个目标的多种配置要并列可比**（本次：5 臂冷编 + 4 臂内循环，全部全新 target dir、串行）；
2. **要报关键路径份额**，而不是只看总耗时——例如"去掉这条链会怎样"的反事实，本机值 25%，这是 `duration_ms` 一个数字看不出来的；
3. **要能证明复用真的发生**：`sccache --zero-stats` → 构建 → `--show-stats`（本次直接抓到"显式 `CARGO_TARGET_DIR` ⇒ 0 命中"这个反直觉结论）；
4. **磁盘口径要 du 优先**（本次 `debug=0` 的 372 M→225 M 是 du；逻辑字节会给出不同结论）。

---

## 附：本次实验的原始数字

```
verify-gate 冷编（161 包 / 86 crate / 112 unit，10 核 M1 Pro）
  baseline stable dev          wall 8.27  cpu 45.95  target 372M
  debug=0                      wall 7.54  cpu 39.19  target 225M
  nightly baseline             wall 8.06  cpu 43.12  target 303M
  nightly -Zthreads=8          wall 7.21  cpu 50.45  target 303M
  nightly -Zthreads=8 +debug=0 wall 5.96  cpu 45.71  target 158M
内循环（touch src/main.rs）
  baseline 0.91 | debug=0 0.78 | -Zthreads=8 0.62 | 组合 0.61
结构
  前端 26.9s / 后端 8.7s；平均并发 5.56/10；t=0 可跑 34，t>=6s 只剩 1-3
  反事实：去掉 ureq/rustls/icu/url/idna 链 → 6.00s（该链 2.01s = 25%）
  warm no-op 0.07s；touch 后仅 1 个 crate 重编 0.88s
sccache 跨项目（rustopt ↔ run-diff，副本）
  版本不一致+默认 target/   hits 5   wall 3.99
  版本对齐+默认 target/     hits 21  wall 2.75
  版本对齐+二次             hits 22  wall 2.28
  版本对齐+显式 target dir  hits 17  wall 3.79
```

---

## 8. 已落地（2026-10-07 实际改动）

原始执行日志：[docs/evidence/step-a-version-alignment.log](evidence/step-a-version-alignment.log)、[docs/evidence/step-bd-debuginfo-and-sccache.log](evidence/step-bd-debuginfo-and-sccache.log)
改动前副本（回滚用）：[docs/evidence/rollback/](evidence/rollback/)

### 8.1 A —— 版本对齐（7 个仓，只动 `Cargo.lock`）

| 项目 | 改动 |
|---|---|
| fmtguard / run-diff / sandbox-run | `syn 3.0.3→3.0.6`、`unicode-ident 1.0.24→1.0.26` |
| session-index | `unicode-ident`、`syn(3.0.x)→3.0.6` |
| unirun | `unicode-ident`、`syn(3.0.x)→3.0.6`、`cc 1.4.3→1.4.4`、`icu_provider 2.3.0→2.3.1`、`zerovec-derive 0.11.5→0.11.6` |
| verify-gate | `unicode-ident`、`syn(3.0.x)→3.0.6` |
| rustopt | 无需改动（本来就是 `syn 3.0.6` / `unicode-ident 1.0.26`） |

**结果：可对齐的跨项目版本分叉 14 → 0。**
仍剩 **13 个跨不兼容族群**的重复（`sha2` 0.10/0.11、`syn` 2/3、`hashbrown` 0.14/0.17、`windows-sys` 0.52/0.61、`rand` 0.8/0.10、`getrandom` 0.2/0.4、`webpki-roots` 0.26/1.0、`const-oid`、`cpufeatures`、`crypto-common`、`digest`、`rand_core`、`block-buffer`）——这些只能靠升级上游依赖解决，不是对齐问题。

**验证：7/7 `cargo build --locked` ✓、7/7 `cargo test --locked` ✓**（fmtguard 19、run-diff 7、rustopt 43+9、sandbox-run 35、session-index 2、unirun 10 组共 ~139、verify-gate 24），**且 rustopt 的 MSRV `cargo +1.87.0 check --offline --all-targets` ✓**。

顺带查清两件事：

* **verify-gate 的 `syn 2.x` 在构建路径内**：`synstructure ← yoke-derive ← yoke ← icu_collections ← icu_normalizer`（ICU 栈，来自 `ureq→url→idna`）⇒ 每次冷编多付约 **2.5 s**，而且它就在那条值 25% 的关键路径尾巴上。要消除必须升级 `ureq`/`url`/`idna` 生态（产品决策）。
* **unirun / session-index 的 `syn 2.x` 只在 dev/bench**（`criterion→ciborium→zerocopy→zerocopy-derive`）⇒ `cargo build` 不付这个成本。**锁文件里的重复 ≠ 参与构建的重复**，判断"该不该合并版本"要按 `cargo tree -e normal` 看。

### 8.2 B —— verify-gate 试点 dev 关 debug info

改动：`verify-gate/Cargo.toml` 的 `[profile.dev]` 增加 `debug = 0`（附实测数字注释）。

| 指标 | 改前 | 改后 |
|---|---|---|
| 冷编墙钟（全新 target dir） | 8.27 s | **7.30 s** |
| 全新 target 体积 | 372 M | **225 M** |
| 内循环（改一行） | 0.91 s | 0.84 s |
| 测试 | — | 24 passed ✓ |

**踩到的坑（重要）**：在**已有** target dir 上切换 profile **不会回收旧产物**——新旧按不同 hash 并存，verify-gate 的 target 从 782 M 涨到 **990 M**。本次 `cargo clean` 清掉 4,476 文件 / 1,018 MiB，重建后 **225 M**。
⇒ **"改配置省磁盘"必须配一次清理才算数**；只想回收被取代的产物而不想重编，用配套文档 §5.2 的细档工具。
（想保留 backtrace 的 `file:line` 就用 `debug = "line-tables-only"`：实测 7.91 s / 309 M。）

### 8.3 D —— 接线 sccache

改动：新建 `~/.cargo/config.toml`：

```toml
[build]
rustc-wrapper = "sccache"
[env]
SCCACHE_IGNORE_SERVER_IO_ERROR = "1"   # 服务端 IO 错误时退回本地编译，不让构建失败
```

**没有删除任何 `CARGO_TARGET_DIR`**——全机本来就没有任何地方设置它（7 个仓都没有 `.cargo/config.toml`；唯一会设它的是 `rustopt` 自己的代码，那是它"按变体隔离 target dir"的设计，见 §4 的取舍）。

前置验证（**推翻了一条流传的说法**）：**`RUSTC_WRAPPER` 不进 cargo 指纹**。同一 target dir 交替"带 wrapper / 不带 wrapper"，不碰源码时都是 **0 重编** ⇒ **接线本身零重编成本**。（`cargo-orphan-gc` 的 README 称会触发一次全量重编；本机 cargo 1.97 实测不是。）

真实端到端（真仓，清掉两个小仓的 target 后冷编）：

| 步骤 | 墙钟 | 命中 / 未命中 |
|---|---|---|
| fmtguard 冷编 | 3.96 s | 0 / 13 |
| run-diff 冷编（复用 fmtguard 的产物） | **2.73 s** | **9** / 13 |

代价（必须说清）：

* **未命中路径变慢**：小仓对照 4.44 s（无 sccache）→ 5.03 s（全 miss）≈ **+13%**；**命中路径 −30%**（→3.09 s）。收益来自"第二次 / 跨项目 / 跨机器"，**第一次冷编是净亏**。
* **你正在改的那个 crate 永远不进缓存**：非缓存原因 `crate-type 18 / missing input 4`（bin 一律不缓存；带 `-C incremental` 的也不缓存）。
* 缓存目录 350 M，上限 10 GiB（`SCCACHE_CACHE_SIZE` 可调）。
* 回滚：删除 `~/.cargo/config.toml`。

> **⚠️ 后续修正（同日两轮补测，务必一起读）**：sccache 的墙钟收益**完全取决于"key 是否匹配"**，不是固定倍数。
>
> | 条件 | 墙钟 | CPU(user+sys) | sccache |
> |---|---|---|---|
> | **keys 全匹配（缓存全热）** | **3.29 s** | 9.4 s | **106 命中 / 0 未命中** |
> | 全冷（无 wrapper） | **6.97 s** | 38.61 s | — |
> | 部分匹配（同 session 早期状态，缓存未覆盖全部 key） | 8.27 s | 9.48 s | 总命中率 61.3%（Rust 50% / C 100% / asm 100%） |
>
> 读法：**keys 匹配时 −53% 墙钟 / −76% CPU；不匹配时 0 命中、墙钟不改善甚至略慢**。上面 rustopt 小仓的 −30% 是同一规律的另一种取值。
> 原因：① 命中率 = `P(H(源, 参数, 路径, 环境) 相同)`，版本/路径/`CARGO_*` 任一变化都会改变它；② 即使整体命中率很高，**关键路径上剩下的仍可能是不可缓存单元**（bin 一律不缓存、build script 执行、链接），所以 tail-bound 项目的收益上界 = 可缓存部分在关键路径上的占比（Amdahl：25% 串行 ⇒ 极限 6.00 s）。
> **所以 sccache 要按三条口径分开讲**：CPU/能耗/CI 成本 ↓↓（−76%）、**墙钟 = f(命中率)**（匹配 −53% / 不匹配 0）、跨项目/跨机器 ↓↓ 有效。详见 [optimization-space.md](optimization-space.md) §7 与 [FINDINGS.md](FINDINGS.md) §2、§4.3。

### 8.4 未做（按你的选择）

* **C（nightly + `-Zthreads=8`）**：未落地；收益 −12.8% 墙钟 / 内循环 −32%，命令见 §6，代价是锁 nightly、CPU +17%。
* **结构级去依赖**（去掉 `ureq→rustls→icu` 链，值 25% 墙钟）：属产品决策，未动。
* **CI 侧**：7 个仓中 5 个已有 `Swatinem/rust-cache`，但都**没有** sccache。GitHub Actions 的工作区路径跨运行固定，正是 sccache 最有效的场景，值得后续加 `sccache-action`。

### 8.5 回滚

| 改了什么 | 怎么回 |
|---|---|
| 7 个仓的 `Cargo.lock`（+ verify-gate 的 `Cargo.toml`） | 用 [docs/evidence/rollback/](evidence/rollback/) 里的同名副本覆盖（另有 `/tmp/opt-backup-1791321266/`） |
| `~/.cargo/config.toml` | 删除该文件 |
| verify-gate 的 `debug = 0` | 删掉那一行，并再清一次 `target/` |
