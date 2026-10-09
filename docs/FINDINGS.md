# 发现记录（Findings & Verdicts）

2026-10-07 · 机器：Apple M1 Pro（8P+2E / 10 逻辑核）· rustc/cargo **1.97.0 stable** + **1.100.0-nightly** · sccache **0.17.0**

配套文档：[bottleneck-verified.md](bottleneck-verified.md)（瓶颈实测与已落地改动）、[optimization-space.md](optimization-space.md)（方案空间：工程/数学/物理/跨语言）、[compile-speed-disk-cache.md](compile-speed-disk-cache.md)、[low-level-and-cross-language.md](low-level-and-cross-language.md)
原始日志：[docs/evidence/](evidence/)　回滚副本：[docs/evidence/rollback/](evidence/rollback/)

---

## 0. 一句话结论

| 问题 | 结论 |
|---|---|
| **有没有"显著且稳定"的构建加速方案？** | **没有单一的。** 稳定档只能叠加到 **~10–20%**（依赖 debug info 剥离 + 缓存 + 版本对齐）；显著档（`-Zthreads` 的 **−12.8%**）**要 nightly**；而**最大的一块（verify-gate 的 25%）在依赖图里**，属产品决策，不是配置能解决的 |
| **能不能稳定清理项目体积？** | **能清理，且判据是机械的**（全机可回收 **5.59 GB** du 口径；verify-gate **999 M → 233 M** 已兑现）。但"稳定不反弹"要**同时改 6 个仓的配置**；而**跨项目重复的 ~4 GB 只能靠版本对齐**，其中 13 条不兼容族群无法合并 |
| **天花板在哪？** | 四条结构性事实：① 墙钟由**关键路径**决定（Amdahl：verify-gate 串行尾巴 25% ⇒ 极限 6.0 s）；② 复用的最小粒度是 **crate**（无稳定 ABI/接口）；③ **没有内容寻址的全局缓存**（cargo 的 target 是可变工作区，sccache 是外挂且 key 含环境变量）；④ 单 unit 前端**单线程**、并行化仅 nightly。这四条都不是本地配置能突破的 |

---

## 1. 本文对"稳定"的定义

**稳定性判据（五条全满足才算稳定）**：① 不依赖 nightly；② 不改指纹、不依赖缓存热度；③ 可重复（同条件多次测量一致）；④ 跨项目通用；⑤ 失败模式可控且可回滚。
**显著性门槛**：墙钟 ≥10% 且可重复。（低于这个值不算"显著"，但可以叠加。）

---

## 2. 判定表（核心交付）

| 方案 | 显著？（实测墙钟） | 稳定？ | 成本 / 风险 | 结论 |
|---|---|---|---|---|
| **依赖 debug info 剥离**（`[profile.dev.package."*"] debug=0`） | ❌ −11.7%（8.27→7.34 s）；磁盘 **−40%**（372→225 M） | ✅ 五条全满足 | 依赖不可调试（自身 crate 仍可） | **默认策略采用**（已落地 verify-gate，实测 233 M / 内循环 0.87 s） |
| **sccache 接线**（全局 `rustc-wrapper`） | ✅ 但当且仅当 key 匹配：**3.29 s vs 6.97 s（−53%）**；key 不匹配时 **0% 命中**（8.27 s vs 7.38 s） | ⚠️ 依赖 key 稳定（版本/路径/feature/环境）；不改指纹 | 未命中路径 +13%；bin/incremental 不缓存 | **保留**，但按三条口径分开报（墙钟/CPU/跨项目） |
| **版本对齐**（可对齐分支 14→0） | ✅ 结构性；跨项目复用 5→21 unit（受控实验），真实两仓 fmtguard→run-diff 9 命中 | ✅ | 锁文件变更（已有备份/回滚） | **已完成并验证**（7/7 build+test ✓，MSRV ✓） |
| **`-Zthreads=8`**（并行前端） | ✅ −12.8% 墙钟、内循环 −32%，CPU +17% | ❌ 要 nightly；切换工具链 ⇒ 全量重编（实测 12/12）；产物并存翻倍 | 锁 nightly、CI 分裂、release 不可用 | **不进默认**；按项目可选（钉日期版） |
| **结构级去依赖**（砍 `ureq→rustls→icu` 链） | ✅ 上界 **25%**（反事实 8.01→6.00 s） | ⚠️ 稳定但**不是配置**：要改依赖与代码 | 产品/安全取舍 | **按项目决策**；verify-gate 的 `syn 2.x` 就在这条链上 |
| **7 仓合并 workspace** | ✅（统一版本与图） | ⚠️ | 成员 `[profile.*]` 被忽略；`session-index` 的 release 是 `opt-level="z"` 与另 6 个冲突 | **只在 profile 政策一致的子集做** |
| **`-j` / 核拓扑调优** | ❌ 噪声（−j8 7.42 s / −j10 7.38 s / −j12 7.15 s） | ✅ | — | **不投**（系统处于欠载区 ρ<1） |
| **换链接器** | ❌ macOS 27 无可用替代（lld 不能解析 SDK；`-ld_classic` 已移除；mold/wild 是 ELF-only） | — | — | **不投** |
| **`cargo -Zgc` / build-dir GC** | ❌ 1.100-nightly 无可观察行为 | — | — | **观察**（build-dir 新布局本身有用：`target/` 与 `build/` 分家） |
| **`cargo check` 内循环 + RA 独立 target dir** | ✅ 2–3×（不产出二进制的场景） | ✅ 但 RA 设 `CARGO_TARGET_DIR` 会牺牲 sccache 跨项目复用 | 需接受 check 漏 codegen 期诊断 | **推荐按场景使用** |
| **清理 target（incremental / superseded）** | ✅ 全机 **5.59 GB** du 口径可回收 | ✅ 机械判据 + 年龄门 + flock 闸门 + 台账（既有工具） | 无（回滚 = `cargo build`） | **采用**；缺"防再生"就白清 |
| **清理 `~/.rustup` 无引用工具链** | ✅ **5.85 GB** | ⚠️ 需人工确认例外（`stable` 是 default；`1.87.0` 是 rustopt 的 MSRV） | 需联网可重装 | **采用（按例外清单）** |
| **清理 `~/.cargo/registry/src`** | ✅ **1.9 GB** | ✅ 可从 `cache/*.crate` 重新解包 | 首次需解包/联网 | **采用** |

---

## 3. 被实测推翻的说法（记录下来，避免再走一遍）

| 流传的说法 | 本机实测 |
|---|---|
| "换个链接器（lld/mold）能显著提速" | macOS 27 上 lld 连 SDK 的 `.tbd` 都解析不了；`-ld_classic` 已移除；mold/wild 是 ELF-only ⇒ **无路** |
| "加 `RUSTC_WRAPPER` 会触发一次全量重编" | **0 重编**（交替带/不带 wrapper，不碰源码均 0）⇒ 接线零指纹成本 |
| "sccache 天生跨项目复用" | **只在没有显式设置 `CARGO_TARGET_DIR` 时**；把该变量设成**与默认完全相同的路径**也会掉一半（21→17 命中） |
| "`SCCACHE_BASEDIRS` 能解决路径差异" | 服务端生效后仍 **0 命中**，且同目录重编变得不稳定 |
| "cargo `-Zgc` 能自动回收 target" | 1.100-nightly 上探针目录与 `~/.cargo` 顶层**都没有任何 state/回收行为** |
| "限制在 P 核（`-j 8`）会更快" | **无差异**（7.42 vs 7.38 vs 7.15 s，CPU 一致） |
| "锁文件里的重复 = 参与构建的重复" | `syn 2.x` 在 unirun/session-index **只是 dev/bench**（`cargo tree -e normal` 里没有）；只有 verify-gate 在构建路径上（ICU 链） |
| "接了缓存墙钟一定变快" | **取决于 key 命中率**：全匹配 −53%，部分匹配时 8.27 s vs 7.38 s（反而略慢） |
| "改 profile 就能省磁盘" | 在**已有** target dir 上改 profile，新旧产物并存 ⇒ verify-gate 782 M → **990 M**；必须清一次（`cargo clean` 掉 1,018 MiB），重建后 233 M |

---

## 4. 实测数据总表（可引用基线）

### 4.1 代表性项目结构（verify-gate）
161 个包 → 编译 **86 个 crate** → 图内 **112 个 unit**；`W = 38.6–45.9 s` CPU；`L ≈ 7.0–8.3 s` 墙钟；**平均并发 5.56/10（56%）**；**前端 : 后端 = 76 : 24**；并发曲线 t=0 有 34 个可跑、t≥6 s 只剩 1–3；**反事实：去掉 `ureq/rustls/icu/url/idna` 链 → 6.00 s（该链 = 25%）**；链接含在最后 0.79 s 内。

### 4.2 配置对照（verify-gate，各全新 target dir，串行）
| 配置 | 冷编墙钟 | CPU | target | 内循环 |
|---|---|---|---|---|
| 默认（debug=2） | 8.27 s | 45.95 s | 372 M | 0.91 s |
| `dev.debug=0` | 7.57 s | 39.2 s | 225 M | 0.87 s |
| **`dev.package."*".debug=0`（已采用）** | **7.34 s** | — | **225 M**（落盘后 233 M） | **0.79 s** |
| `dev.debug="line-tables-only"` | 7.17 s | — | 309 M | 0.78 s |
| nightly `-Zthreads=8` | 7.21 s | 50.45 s | 303 M | 0.62 s |
| nightly `-Zthreads=8` + `debug=0` | **5.96 s** | 45.71 s | 158 M | **0.61 s** |

### 4.3 缓存（同一配置、keys 匹配 vs 不匹配）
| 条件 | 墙钟 | CPU | sccache |
|---|---|---|---|
| **keys 匹配（全热）** | **3.29 s** | 9.4 s | **106 命中 / 0 未命中** |
| 全冷（无 wrapper） | **6.97 s** | 38.61 s | — |
| 部分匹配（同 session 早期状态） | 8.27 s | 9.48 s | 总命中率 61.3%（Rust 50% / C 100% / asm 100%） |

### 4.4 跨项目复用
| 实验 | 结果 |
|---|---|
| fmtguard → run-diff（真仓，默认 target dir） | run-diff 冷编 **2.73 s / 9 命中**（fmtguard 3.96 s / 0 命中） |
| rustopt ↔ run-diff（/tmp 副本，版本不一致） | 5 命中 / 17 |
| 同上，版本对齐后 | **21 命中 / 22**，墙钟 3.99 → 2.75 s |
| 同上，但显式 `CARGO_TARGET_DIR`（路径与默认相同） | 掉到 17 命中 |

### 4.5 磁盘账（du 口径）
| 项 | 数值 |
|---|---|
| 全机 14 个 target 合计 | ~26.5 GB（cantool 11 G / cankey 7.6 G / canpad 4.6 G / unirun 2.0 G …） |
| **机械判据可回收** | **5.59 GB**（incremental 5.16 G + superseded 0.44 G） |
| 仍在再生 incremental 的仓 | 6 个（canpad、cantool/src-tauri、cc-switch/src-tauri、espanso、phone-geo-lookup/src-tauri、qsv2flv） |
| `~/.rustup` | 10.34 GB，其中**无引用 5.85 GB**（例外：`stable` 是 default、`1.87.0` 是 MSRV） |
| `~/.cargo` | 2.4 GB（`registry/src` **1.9 GB**、`cache` 263 M、`index` 68 M、`advisory-dbs` 45 M） |
| 跨项目 rlib 重复 | 2,071 个 / **4.05 GB**；`syn` 42 份、`quote` 30、`serde` 28 |
| 版本分叉（全机 24 个 lock） | 8,723 条 (crate,version)、669 个 crate 多版本、**1,484 条冗余分支**；dsfolder 7 仓**可对齐分支 14 → 0** |
| verify-gate 兑现 | 999 M（并存）→ `cargo clean` → **233 M** |

---

## 5. 变更台账（本机已改 + 回滚）

| 变更 | 范围 | 验证 | 回滚 |
|---|---|---|---|
| `syn → 3.0.6`、`unicode-ident → 1.0.26`（+ unirun 的 `cc`/`icu_provider`/`zerovec-derive`） | 7 个仓的 `Cargo.lock` | 7/7 build ✓、7/7 test ✓、rustopt MSRV(1.87) ✓ | 用 [docs/evidence/rollback/](evidence/rollback/) 覆盖 |
| `[profile.dev] incremental = false`（原本已有） + **`[profile.dev.package."*"] debug = 0`** | `verify-gate/Cargo.toml`（试点） | build ✓、24 tests ✓、target 233 M、内循环 0.87 s | 恢复 [rollback/verify-gate.Cargo.toml](evidence/rollback/verify-gate.Cargo.toml) |
| `[build] rustc-wrapper = "sccache"` + `[env] SCCACHE_IGNORE_SERVER_IO_ERROR = "1"` | **全局** `~/.cargo/config.toml`（新建） | 真仓跨项目 9 命中；热缓存 −53% 墙钟 | 删除该文件 |
| `cargo clean`（兑现 profile 切换的磁盘收益） | verify-gate `target/` | 999 M → 233 M | `cargo build` 重建 |

为保持干净，我**没有**动这 6 个仓的 incremental 配置、没有删任何工具链、没有动 rustopt 自己的 per-variant target dir 设计。

---

## 6. 未决 / 需要决策

1. **6 个仓加 `[profile.dev] incremental = false`** → 止住 5.16 GB 再生（其中 cantool/src-tauri 4.56 GB 是单点最大）。风险：本地单 crate 重编 0.29→0.69 s。
2. **`~/.rustup` 5.85 GB 无引用工具链**：需人工确认 `stable`（当前 default）与 `1.87.0`（MSRV）的处理方式。
3. **CI 接 sccache**：5/7 个仓已有 `Swatinem/rust-cache`，没有 sccache；GH Actions 路径跨运行固定，正是命中率最高的场景。
4. **verify-gate 的 `syn 2.x`（ICU 链）与 `sha2` 族群**：要升级 `ureq/url/idna` 与 `sha2` 系才能合并；属产品决策。
5. **rustopt 自身**：per-variant target dir 与 sccache 天然冲突（变体间永不复用）——"同一 target dir 放全部变体"是否更快，需要用 `--timings` 实测（这正是 rustopt 该量的东西）。
6. **跨机二级缓存**：`SCCACHE_MULTILEVEL_CHAIN` + NAS 上的 S3/WebDAV，让 M1↔M3 共享依赖产物（未实测）。

---

## 7. 触发条件（什么时候要重跑这张判定表）

* cargo **cross-workspace cache** 落地（内容寻址、跨 workspace 复用）→ 会改变 §2 中"缓存"与"版本对齐"的相对权重；
* **并行前端稳定**（`--jobs-frontend` 进入 stable）→ `-Zthreads` 那条从"不稳定"变"稳定"，判定翻转；
* **build-dir 新布局稳定/默认** → 清理策略可改为按目录（`build/` vs `target/`）；
* Rust 出现**稳定 ABI / 更细接口**，或 macOS 出现可用链接器 → 重新评估结构级方案；
* 依赖图发生大改（换 HTTP/TLS 栈）→ 重算 `L`。
