# 编译加速 / 磁盘瘦身 / 缓存复用 —— 方案分析与本机实测

调查日期 2026-10-06/07 · macOS（Apple Silicon）· rustc/cargo **1.97.0** · sccache **0.17.0**
扫描根 `~/syncfolder/project`（14 个 `target/`）与 `~/.cargo`、`~/.rustup`、`~/.cache/rustopt`

> **后续更新**：代表性项目的瓶颈实测与优化对照（冷编 5 臂 + 内循环 4 臂 + sccache 双向验证）见 [bottleneck-verified.md](bottleneck-verified.md)——其中的实测数字**取代**本文 §2.1 表中来自外部调研的估计值。

三条贯穿全文的纪律（来自 `build-artifact-retention` 与既有工具 `scripts/rust-target-audit.py`）：

1. **口径先行**：只有 `du`（按 inode 去重）能回答"删了释放多少"。逐路径 `st_size` 求和会重复计数。
2. **判据机械**：不说"看起来像垃圾"。同 `(crate, ext)` 只留最新 + 年龄门、或"宿主 triple 之外的目录"这类可机械证明的判据。
3. **回滚一句话说清**：本文所有可删项的回滚路径都是 `cargo build`。说不清回滚的，不进清单。

---

## 0. 摘要

| 你问的 | 最值得做的三件事 | 依据（本文实测） |
|---|---|---|
| **加速编译** | ① 先修"不该发生的重编"（环境/RUSTFLAGS/工具链漂移）；② `cargo check` + rust-analyzer 独立 target dir 做内循环；③ 按需用 nightly 的 `-Zthreads` | 本文 §2；RUSTFLAGS 抖动实测 0.63 s → 4.31 s |
| **缩减磁盘** | ① 给仍在再生 incremental 的 6 个仓关掉（最大一项 cantool/src-tauri **4.56 GB**）；② `~/.rustup` 无引用工具链 **5.85 GB**；③ `~/.cargo/registry/src` **1.9 GB** | 本文 §3；审计工具只读预览：**可回收 5.59 GB** |
| **缓存复用** | ① **保持 `CARGO_TARGET_DIR` 路径稳定**——换路径 = sccache 0 命中；② CI 用路径稳定的缓存；③ 关 incremental 否则 sccache 完全不参与 | 本文 §4；同目录重编 **12/12 命中**，换目录 **0/24** |

**最重要的一条**：本机 `target/` 合计约 **26.5 GB**，但可安全回收的只有 **5.59 GB（21%）**——剩下的就是工作集。
真正的大头在 `target/` 之外：`~/.rustup` **10.34 GB**（其中 5.85 GB 无任何仓引用）、`~/.cargo` **2.4 GB**、`~/.cache/rustopt` **415 MB**、sccache 本地缓存 **331 MB**（命中率 0）。
磁盘现状：数据卷 460 GiB，已用 339 GiB，**剩 93 GiB（79% 已用）**——不是火警，但清理的收益是真的。

---

## 1. 本机实测基线

### 1.1 target 总账（14 个目录，约 26.5 GB）

| 项目 | du | 项目 | du |
|---|---|---|---|
| `cantool/src-tauri` | **11 GB** | `dsfolder/fmtguard` | 227 MB |
| `cankey` | **7.6 GB** | `dsfolder/sandbox-run` | 170 MB |
| `canpad` | **4.6 GB** | `dsfolder/rustopt` | 141 MB |
| `dsfolder/unirun` | **2.0 GB** | `dsfolder/session-index` | 141 MB |
| `dsfolder/verify-gate` | 420 MB | `dsfolder/run-diff` | 115 MB |
| `qsv2flv` | 65 MB | `espanso` / `cc-switch/src-tauri` / `phone-geo-lookup/src-tauri` | ≈0 |

其中 7 个 dsfolder 仓**都已** `[profile.dev] incremental = false`（已核对配置文件，不是按目录判断）。残留仅 `run-diff/target/debug/incremental` 15 MB。

### 1.2 三种口径：决定"删了能省多少"

| 对象 | 逐文件逻辑 | `du`（inode 去重） | 虚高倍数 |
|---|---|---|---|
| `rustopt/target`（627 文件 / 627 inode） | 138.7 MB | 140.6 MB | **1.01×**（块对齐，反而 du 略大） |
| `unirun/target`（19 053 文件 / 12 043 inode） | 2.18 GB | **1.98 GB** | **1.10×** |
| └ `unirun` `debug/deps` 的 `.o`（13 866 路径 / 6 856 inode） | 731.9 MB | **516.0 MB** | **1.42×** |
| └ 同类 `.rlib`（247/247）、`.rmeta`（518/518）、`.dylib`（23/23） | — | — | 1.00×（无共享） |

结论：**本机不是"虚高 3–4 倍"那种场景**（cankey 的历史教训是 4.2×），唯一明显虚高的是 `.o` 类，而 `.o` **恰恰不能删**——它是 cargo 重链接的输入（详见 §3.2）。任何只报逻辑字节的工具在本机都会把 `.o` 的收益高估 42%。

### 1.3 缓存、工具链、工具盘点

| 路径 | 占用 | 备注 |
|---|---|---|
| `~/.rustup` | **10.34 GB** | 10 个工具链；审计工具判定 **5.85 GB 无任何仓引用** |
| `~/.cargo` | 2.4 GB | `registry/src` 1.9 GB（1 497 个已解包 crate）、`registry/cache` 263 MB、`registry/index` 68 MB（**sparse**，有 `.cache/`）、`advisory-dbs` 45 MB、`bin` 138 MB |
| `~/.cache/rustopt/work` | 415 MB | 2 个目录，其中 `rustopt-0-1-0-92133a21` 是 manifest 指向临时目录时留下的**孤儿** |
| `~/Library/Caches/Mozilla.sccache` | 331 MB | **命中率 0**（43 次请求 / 0 次执行，见 §4.2） |

已装工具：`sccache` 0.17.0、`cargo-sweep`、`cargo-nextest`、`lld`（Homebrew，**在本机不可用**，见 §2.2）。
未装：`mold`（且 mold 只支持 ELF，macOS 无关）、`hyperfine`、`kondo`、`cargo-cache`。
`~/.cargo/config.toml` **不存在**，无任何项目有 `.cargo/config.toml`，无 `RUSTFLAGS`/`CARGO_*` 环境变量——**没有任何缓存加速被接线**。

### 1.4 工作集健康度

| 场景 | 结果 |
|---|---|
| `rustopt`：`cargo build --release` 连跑两次 | 4.24 s → **0.01 s**（工作集健康，no-op 无代价） |
| `unirun`：`touch src/main.rs && cargo build`（依赖树 2.0 GB 的仓） | **1.12 s**（38 个 unit 的图，只重编叶子 crate） |
| `rustopt`：全新 target dir 冷编（serde + serde_json + 自身，release） | **4.44 s** |

即：**内循环不是瓶颈，冷编译和缓存复用才是**。

---

## 2. 加速编译

### 2.0 先修"不该发生的重编"，再谈加速

指纹抖动是最大的浪费源，而且修它免费。实测两面：

| 场景 | 结果 |
|---|---|
| `rustopt` 改一行后重编（incremental 关，本仓现状） | **0.69 s** |
| 同上但 `incremental = true` | **0.29 s**（**2.4×** 快） |
| 在已热的树上多设一个 `RUSTFLAGS` | **0.63 s → 4.31 s**（全依赖重编：指纹悬崖） |
| `debug = 0`（dev） | 产物 52 MB → **40 MB（−23%）**；小 crate 上时间差在噪声内 |

诊断命令（cargo 的 FAQ 写的是旧路径，当前正确目标在 `core` 下）：

```sh
CARGO_LOG=cargo::core::compiler::fingerprint=info cargo build -vv 2>&1 | grep -i dirty
```

常见"脏"来源：`RUSTFLAGS` 优先级反转（`CARGO_ENCODED_RUSTFLAGS` > `RUSTFLAGS` > `[target.*].rustflags` > `[build].rustflags`，前者会**静默丢弃**后者）、工具链切换、feature 集合变化、`build.rs` 的 `rerun-if-changed` 写错、rust-analyzer 与 cargo 抢同一个 target dir。

> **本仓的具体隐患**：`rustopt` 没有 `rust-toolchain.toml`，本地默认 `stable`，而 CI 钉 `1.97.0`。今天两者恰好都是 1.97.0，一旦 `stable` 前进，本地与 CI 的指纹同时全变。**建议加 `rust-toolchain.toml` 钉 `1.97.0`**，并顺手释放 `stable` 那 1.72 GB（见 §3.4）。

### 2.1 汇总表

| 手段 | 期望收益 | 代价 / 风险 | 通道 | 本机可用性 |
|---|---|---|---|---|
| `debug = 0` 或 `"line-tables-only"`（dev） | 增量重编 30–40%；**本机 verify-gate 实测：墙钟 −8.8%、CPU −15%、磁盘 −40%（372 M→225 M）** | 断点/回溯质量下降 | stable | ✅ 立即可用 |
| `cargo check` 做内循环 | 2–3× | 漏掉只在 codegen 阶段报的错 | stable | ✅ |
| rust-analyzer 独立 `cargo.targetDir` | 消除 RA↔cargo 互相驱逐 | 产物重复 | stable | ✅ |
| incremental **本地开、CI 关** | 单 crate 重编 2.4×（本机实测） | target 膨胀；**sccache 完全失效** | stable | ⚠️ 与"省磁盘/sccache"冲突，见 §3.5 |
| 依赖 feature 统一（hakari / 固定 feature 集） | 消除"随机重编"；顺带省磁盘 | 维护生成的 crate | stable | ✅ 本机痛点明确（§4.3） |
| `-Zthreads=8` 并行前端 | 外部调研 20–30%+（有的程序无收益）；**本机 verify-gate 实测 −12.8% 墙钟、CPU +17%**（见 [bottleneck-verified.md](bottleneck-verified.md)） | 内存/CPU↑，需 nightly | nightly | ⚠️ nightly |
| Cranelift 后端 | **约 5%** 总编译时间（不是传说中的 5–10×） | 无 debug info、SIMD 不完整 | nightly | ⚠️ 收益低于预期 |
| `-Zhint-mostly-unused` | 特定巨型 API 依赖 −23%…−51% | 大范围使用会**变慢** | nightly | ⚠️ 需逐依赖调 |
| **换链接器** | Linux：rust-lld 1.90 起默认（ripgrep 上链接 7×、端到端 −40%）；mold/wild 更快 | — | stable/Linux | ❌ **macOS 无此杠杆**（§2.2） |
| 工具链升级 | 每两个月 1–5%（rustc-perf 2026-07→09 均值 −4.57%） | 偶发回归 | stable | ✅ 已在 1.97 |

### 2.2 macOS 上链接器这条路是堵死的（重要纠正）

网上流行的 `-fuse-ld=lld` 配方在本机**不可用**，实测：

* 本机 `ld -v` → Apple ld-prime `27037.1`，`clang` 已经在用 Apple 的新链接器；`-ld_classic` 直接报 "no longer supported and will be ignored"。
* `clang -fuse-ld=lld` → `ld64.lld: error: could not load TAPI file … libSystem.tbd: malformed file … unknown target arm64e.x1-macos`（Homebrew 的 ld64.lld 解析不了 macOS 27 SDK 的 `.tbd`）。
* rustc 直接指向 `rust-lld`/`ld64.lld` → `__Unwind_GetIP` 未定义 / `-nodefaultlibs` 不认识：`linker=` 替换的是 **cc driver**，不只是链接器。
* mold 和 wild 都是 **ELF-only**；`sold` 作者自己在 README 里建议用 Apple 的链接器。

**所以在 macOS 上不要动 `[target.aarch64-apple-darwin] linker`。** 这条杠杆留给 Linux（CI 的 `ubuntu-latest`）：那里 rust-lld 从 1.90 起已默认（[Rust blog](https://blog.rust-lang.org/2025/09/01/rust-lld-on-1.90.0-stable/)），mold 更快（[mold README](https://raw.githubusercontent.com/rui314/mold/main/README.md)）。

### 2.3 本机结论

按性价比排序：**① 钉工具链 + 固定 RUSTFLAGS 位置（免费）→ ② rust-analyzer 独立 target dir → ③ dev 关 debug info（磁盘+时间双收益）→ ④ 依赖 feature 统一 → ⑤ nightly `-Zthreads`（如果你愿意在 dev 用 nightly）**。
链接器与 Cranelift 在本机**不值得投入**：前者不可用，后者只有 ~5%。

---

## 3. 磁盘瘦身

### 3.1 只读预览（既有工具，`scripts/rust-target-audit.py`，未改动任何文件）

```
宿主 triple: aarch64-apple-darwin；年龄门: 7 天
  incremental         5.16G      9 项   逻辑 5.55G   ← 最大一类
  superseded        440.74M    146 项   逻辑 440.46M
合计可回收（du 口径）: 5.59G
```

明细里最值得注意的两条：

* `cantool/src-tauri` 的 incremental 一项就 **4.56 GB**，且**仍在再生**；
* `cankey` 的 superseded 类 **367 MB**（同 crate 多 hash 的被取代产物）。

**仍在再生 incremental、需要加 `[profile.dev] incremental = false` 的仓（6 个，工具只读报告）**：
`canpad`、`cantool/src-tauri`、`cc-switch/src-tauri`、`espanso`、`phone-geo-lookup/src-tauri`、`qsv2flv`。
加这一行的效果是"上周清完下周不再长回来"——**比清理本身更重要**。

### 3.2 分级与判据

| 类 | 内容 | 判据 | 回滚 | 本机量级 |
|---|---|---|---|---|
| 工作集 | `*/deps` 最新产物 | **永不删**（它把 no-op 压到 0.01 s） | — | 约 21 GB |
| `incremental` | `target/*/incremental` | 永远可删 | `cargo build` | **5.16 GB** |
| 被取代 hash | 同 `(crate,ext)` 非最新 + 年龄门 ≥7 天 | 只认 `.rlib/.rmeta/.dylib/.a`；**刻意不含 `.o`** | `cargo build` | 441 MB |
| 交叉残留 | `target/<非宿主 triple>/` | 宿主 triple 取自 `rustc -vV`；取不到就**一个都不碰** | `cargo build --target …` | 0 |
| flycheck/tmp | `flycheck0`、`tmp` | 永远可删 | 工具重建 | ≈0 |
| **`.o`（明确不清理）** | `deps/*.o` | 硬链接共享 inode：`unirun` 里 13 866 路径 → 6 856 inode，逻辑 731.9 MB 实际只占 **516 MB**，删了却要重编全部 CGU | — | 516 MB |
| release | `target/release` | **看用途**：要出包就留 | `cargo build --release` | — |

> **布局正在变**：cargo 的 `build-dir`（`-Zbuild-dir-new-layout` + `build.build-dir`）把中间产物挪到独立的 `build/`（`incremental/`、`*.o`、build script 的 `OUT_DIR`），`target/` 只留最终产物。**实测**：新布局下 `target/debug/` 里连 `deps/` 都没有了。对清理的含义是"回收粒度终于能按目录划"，但**上表与本文所有工具（含 `rust-target-audit.py`）都按旧布局找路径**，切换前需确认适配。另：`build.build-dir` 是**相对 workspace 根**解析的，不是相对 `target-dir`。

### 3.3 工具选型（先查已装的）

| 工具 | 清什么 | 口径 | 动 `incremental/`？ | 缺口 |
|---|---|---|---|---|
| `rust-target-audit.py`（**已有**） | incremental / superseded / cross-triple / flycheck | **du 优先**，显式打印虚高倍数 | ✅ | 需手动指定 `--class` |
| `cargo-sweep`（已装） | 按 mtime 的旧产物 | ❌ 报**逻辑**字节 | ❌ **完全不动** | 会删"很久没重建但仍在用"的依赖 ⇒ 换来一次重编 |
| `cargo-cache` / `kondo`（未装） | `~/.cargo` 层 | 各自不同 | — | 与 target 无关 |

分工建议：**粗档用 `cargo-sweep`（多省空间、接受一次重编），细档用现有 `rust-target-audit.py`（同 `(crate,ext)` 只删被取代的最旧，不触发重编）。** 不必再造一个工具。

### 3.4 机器级（`target/` 之外，这里才是大头）

| 动作 | 可回收 | 风险与注意 |
|---|---|---|
| `rustup toolchain uninstall` 无引用工具链 | **5.85 GB** | 审计工具列出 1.81、1.81.0、1.87.0、1.88.0、1.95.0、1.96.1 均无仓引用。**两个例外必须人工确认**：`stable` 是当前 **default**（删它要先 `rustup default` 换一个）；`1.87.0` 是 `rustopt` 的 MSRV 作业用的版本，若你要离线复现 MSRV 检查就得留。另注意 `1.95` 被 3 个仓钉住、`1.97.0` 被 cankey/unirun 钉住——**不要只看大小删**。 |
| 删 `~/.cargo/registry/src`（1 497 个解包目录） | **1.9 GB** | 安全：需要时从 `registry/cache/*.crate` 重新解包（263 MB），必要时才联网。 |
| 清 sccache 本地缓存 | 331 MB | 命中率 0 时纯属死重；`sccache --stop-server && rm -rf ~/Library/Caches/Mozilla.sccache`。 |
| 清 `~/.cache/rustopt/work` 孤儿目录 | 188 MB | 用 `rustopt clean`（先预览，加 `--apply` 才删）；注意它目前是**整根删**，不是按目录挑。 |
| 关 6 个仓的 incremental | 止住 5.16 GB 再生 | 代价是本地单 crate 重编从 0.29 s 变 0.69 s（本机实测）。 |

### 3.5 一个真实的取舍：incremental 与 sccache 互斥

* `[profile.dev] incremental = false`：省磁盘（本机最大一类）、让 sccache 有机会工作、构建更可复现；
* `incremental = true`：本地改一行快 2.4×（0.29 s vs 0.69 s），但 sccache 明确拒绝缓存增量单元（本机历史统计里 20 次 `incremental` 不可缓存）。

对**小仓/内循环重**的场景留 incremental；对**CI 与要复用缓存的场景**必须关。两者不要在同一 target dir 里混。

### 3.6 执行纪律（照抄即可）

1. **两段式**：默认只读预览，`--apply` 才动；`--apply` 前先跑同参数预览。
2. **闸门 fail-closed**：用 cargo 自己的构建锁 `target/<profile>/.cargo-lock` 做 `flock` 非阻塞探测；`ps` 只作兜底，`ps` 不可用也拒绝。
3. **删除方式看硬链接**：target 里大量硬链接，挪进废纸篓**不释放 inode**；可再生产物直接删才有意义。
4. **台账**：每次执行追加 JSONL（ts/op/class/items/bytes_du/verdict + 逐项路径），删前复核 `dev/ino/mtime_ns`，预览后被改过就拒绝。

---

## 4. 缓存复用

### 4.1 cargo 自身：什么能让"复用"活下来

本机现状是健康的（§1.4：no-op 0.01 s、dev 循环 1.12 s）。破坏复用的清单（按现场概率排序）：

1. **`CARGO_TARGET_DIR` 变化 / 移动项目目录**（路径进入 unit identity）；
2. **`RUSTFLAGS` 来源漂移**（含"shell 里多了一个 `RUSTFLAGS`"这种静默优先级反转）；
3. **工具链切换**（本机默认 `stable` = 漂移源）；
4. **feature 集合随构建范围变化**（`cargo build -p X` 与 `cargo build --workspace` 的 feature 统一结果不同 ⇒ 同一依赖重编，cargo FAQ 明确列为常见原因）；
5. `build.rs` 的 `rerun-if-changed` 写错 ⇒ 脚本每次重跑 ⇒ 下游全重编；
6. rust-analyzer 与 cargo 共用 target dir 互相驱逐。

### 4.2 sccache 实测：**路径一变，命中率归零**

控制实验（`rustopt`，release，serde + serde_json + 自身，24 个编译请求/轮）：

| 轮次 | 命令 | 结果 |
|---|---|---|
| 1 | 冷编入 `CARGO_TARGET_DIR=/tmp/rs-a` | 0 命中 / **12 miss**（写入 12 条） |
| 2 | 删掉 rs-a 再编入 **同一路径** | **12 命中 / 0 miss（100%）** |
| 3 | 删掉 rs-b，冷编入 **`/tmp/rs-b`** | **0 命中** / 12 miss |
| 4 | 删掉 rs-b 再编入同一路径 | **12 命中 / 0 miss（100%）** |

时间对比：全新目录冷编 **4.44 s**（无 sccache）vs 5.03 s（sccache 冷编，多约 0.6 s 开销）vs **3.09 s**（命中重编，省约 1.35 s / −30%）。

隔离实验（合成 `rustc` 调用，逐项只改一个变量）：

| 只改这一个变量 | 结果 |
|---|---|
| `--out-dir` 不同 | **命中** |
| `-L dependency=` 不同 | **命中** |
| `--extern <name>=<绝对路径>` 不同 | **命中** |
| 无关环境变量 `FOO` 不同 | **命中** |
| **`CARGO_TARGET_DIR` 不同** | **0 命中** |
| **`CARGO_PKG_NAME` 不同** | **0 命中** |
| **`CARGO_HOME` 不同** | **0 命中** |

机制：cargo 会把一大批 `CARGO_*` 交给 rustc（本机 dump 实测：`CARGO_TARGET_DIR`、`CARGO_HOME`、`CARGO_MANIFEST_DIR`、`CARGO_MANIFEST_PATH`、`CARGO_PKG_*`、`CARGO_CRATE_NAME`、`OUT_DIR`、`CARGO_CFG_*`、`CARGO_ENCODED_RUSTFLAGS`），而 sccache 0.17.0 把这些环境变量计入 key；相反，**命令行里的输出/依赖路径会被正常归一化**。所以：

* **依赖**（来自 `~/.cargo/registry/src/index.crates.io-…/<crate>-<ver>`）的 `CARGO_MANIFEST_DIR` 全机同一个绝对路径 ⇒ **只要不设 `CARGO_TARGET_DIR`，跨项目复用依赖编译是可能的**；
* 一旦为了"隔离/统一 target"显式设了 `CARGO_TARGET_DIR`（rustopt 的 per-package/per-variant work dir、CI 里的自定义 target dir、共享 target dir 方案）⇒ **跨目录复用直接归零**；
* 你自己的 crate 因 `CARGO_MANIFEST_DIR` 是项目路径，天然不可跨项目复用（这是对的）。

`SCCACHE_BASEDIRS` 救不了：把服务端也配上后（`sccache --show-stats` 显示 `Base directories /tmp/`），跨目录仍是 **0 命中**，而且同目录重编也变得不稳定（0 命中）。本机不要指望它。

**本机 sccache 现状**：`43 请求 / 0 执行`，不可缓存原因 `incremental 20 / missing input 12 / crate-type 5`。也就是说这台机器上 sccache 到目前为止**一个字节的编译都没省下**（331 MB 缓存目录是纯死重）。

其他已知限制：`bin`/`dylib`/`cdylib`/`proc-macro` 一律不缓存（本机每轮 8 次 `crate-type` 不可缓存）；sccache 与 incremental 天然互斥。

### 4.3 "没有复用"的账单：跨项目 rlib 重复 4.05 GB

全树扫描（`*/target/*/deps/lib*.rlib`，深度 6）：

* **2 071 个 rlib，逻辑合计 4.05 GB**；
* 同名 crate 的多份拷贝：`syn` **42 份**、`quote` 30、`serde` 28、`proc_macro2` 28、`serde_json` 26、`serde_core` 23、`unicode_ident` 22、`memchr` 20、`itoa` 19、`hashbrown` 16、`bitflags` 15、`sha2` 14、`libc` 14…
* 按项目：`cankey` 1 105 MB / `canpad` 1 073 MB / `cantool` 934 MB / `unirun` 641 MB。

其中一部分是**同项目内 feature 组合发散**（`unirun` 里 `syn` 13 份、`toml_edit` 3 份），另一部分是**跨项目各编一遍**。前者用 feature 统一（hakari / 固定 feature 集）能同时省时间和磁盘；后者只有共享缓存能省，而共享缓存的前提正是 §4.2 的路径稳定。

### 4.4 CI

现状（`.github/workflows/ci.yml`）已经用了 `Swatinem/rust-cache@v2`，且 `release` 作业按 `${{ matrix.target }}` 分 key——这部分是对的。可加：

* **`sccache-action`**：GitHub Actions 的工作区路径跨运行/跨机器是**固定**的（`/home/runner/work/...`），所以 §4.2 的路径敏感问题在 CI 里不成立，sccache 才真正有效；
* 保持 **toolchain 钉版本 + `--locked`**（已做），并给本仓补 `rust-toolchain.toml`，让本地与 CI 同指纹；
* `cargo-nextest` 只省**运行**时间（1.37×–3.38×），不省编译；用了它要单独跑 `cargo test --doc`。

### 4.5 rustopt 自身：一个需要测量的设计取舍

`rustopt` 给**每个变体**一个独立 `CARGO_TARGET_DIR`（`~/.cache/rustopt/work/<pkg>-<hash>/<variant>`）。按 §4.2：

* 变体之间**天然不可能**互相复用（args 里的 profile 参数不同，且 target dir 不同）；
* `work_root_for` 用 manifest 的 canonical path 做 hash ⇒ **项目一移动，缓存全废**（本机就留了一个 `rustopt-0-1-0-…` 的孤儿目录）；
* 每个变体的冷编是矩阵成本的来源，这一点 README 已经承认（"the first run is therefore slow"）。

可测量的问题：**把所有变体放进同一个 target dir 会不会更快？**
会，但收益不是来自 sccache（profile 参数进 key，依赖仍会按 hash 重编），而是来自：`build.rs` 输出、`registry` 解包、以及 cargo 的 `.fingerprint` 目录不重复建立。这需要实测，不能估算——正好符合 rustopt 的"measure, never estimate"。

---

## 5. 落到 rustopt 的三个候选功能（按优先级）

### 5.1 `rustopt cache` —— 缓存复用健康检查（优先级最高，成本最低）

一个只读/近只读的子命令，输出"你的构建缓存健康吗"：

* **no-op 探针**：`cargo build` 连跑两次，报告第二次耗时（本机 `rustopt` = 0.01 s，健康阈值可取 0.5 s）；`touch src/main.rs` 后再跑，报告增量重编耗时（本机 `unirun` = 1.12 s）；
* **指纹抖动审计**：读 `CARGO_LOG=cargo::core::compiler::fingerprint=info -vv`，把 `dirty:` 原因归类（EnvVarChanged / RUSTFLAGS / features / mtime），这直接复用 `events.rs` 的"结构化事件 + 台账"套路；
* **环境审计**：是否存在 `RUSTFLAGS`/`CARGO_TARGET_DIR`/`CARGO_PROFILE_*` 环境变量与 `.cargo/config.toml` 冲突（优先级反转）；是否有 `rust-toolchain.toml` 与 CI 钉的版本一致；target dir 是否被显式设置（§4.2 的复用杀手）；
* **sccache 探针**：`sccache --zero-stats` → 两次构建 → 解析 `--show-stats`，直接报命中率与不可缓存原因分布。

验收：`--emit json` 可被 CI 断言；exit code 沿用现有契约（0 健康 / 1 有抖动 / 2 测不出）。

### 5.2 `rustopt target` —— du 优先的产物清理

不要重写 `scripts/rust-target-audit.py`，把它的判据接进 rustopt：

* du 优先（inode 去重）+ 显式打印虚高倍数；`.o` 永不在清单里；
* 分类 `incremental` / `superseded`(+年龄门) / `cross-triple`(宿主 triple fail-closed) / `flycheck`；
* **闸门**：`target/<profile>/.cargo-lock` 的 `flock` 非阻塞探测，fail-closed；
* 两段式 `--apply`，复用 `ledger.rs` 写 JSONL，删前复核 `dev/ino/mtime_ns`；
* 顺带把 `rustopt clean` 目前"整根 work 目录删"的行为改成按目录挑 + 年龄门（本机现有 188 MB 孤儿就是它该抓的）。

> **动手前先读这两个已有实现（2026-10-06 核实）**
>
> 1. **`cargo-orphan-gc`**（crates.io 真实存在：MIT，3,050 行 Rust，2026-08-12 首发、0.8.28 于 08-21，**总下载 38 次**，单作者账号）——
>    它瞄准的正是本节这个缺口，并且理由与本文实测一致：sccache **结构上拒绝** `--crate-type bin` 与 `-C incremental`（本机每轮 8 次 `crate-type` 不可缓存、历史统计 20 次 `incremental`），而 `cargo sweep --maxsize` **从不遍历 `incremental/`**。
>    值得直接吸收的是它的**不变式表**（A 无成功替换不回收 / B 只删学到的所有权，未知则泄漏 / C family 身份保守 / D 当前路径优先 / E 活跃输入加租约 / F 路径校验 fail-closed / G 跨 family 的当前路径优先）和"**dry-run 默认为真**"。它自己记录过一次真实事故：早期版本的 family 身份 bug 删掉了一个正在使用的 `.rmeta`，打断了十代理共享工作区的并发构建 —— 这就是"删文件的编译器 wrapper"为什么必须先用 shadow 模式。
>    代价与限制（作者自述）：装/卸都会改变 cargo 指纹 ⇒ **各一次全量重编**；每次 rustc 调用要走 out-dir（21 万条目时 145 ms/次，`full-scan-every=16` 摊销后 ~6 ms）；未测 Windows；与 `-Zfine-grain-locking` 不兼容；状态目录丢失需重新学习。
>    **对本机的判断**：本机 incremental 总量 5.16 GB（`cantool/src-tauri` 占 4.56 GB），但**更简单的第一步仍是给那 6 个仍再生的仓加 `incremental = false`**——直接把问题消掉，而不是引入一个会改指纹的 wrapper。真要"保留 incremental 又要回收"，它是目前唯一的现成实现，务必先 shadow。
> 2. **Dune（OCaml）的 `dune cache trim --size=BYTES`** 给出同类工具的**硬链接判据**：它把"link count > 1 的缓存条目"定义为**不可回收开销**，因为"裁剪 link count 大于 1 的条目不会释放任何磁盘空间"。
>    这与本文 §3.2 的 `.o` 结论是同一件事（13,866 路径 / 6,856 inode），也是 `rustopt target` 若真要做，必须抄的第一条规则。Dune 还因为"用户规则可能含未声明依赖 ⇒ 陈旧命中"而把缓存默认设成 `enabled-except-user-rules`——**与 `RUSTFLAGS` 漂移是同一类 bug**。

### 5.3 `rustopt plan --time-budget` / Pareto —— 尺寸 × 时间

`duration_ms` 已经在 `measure::Outcome` 里了，`plan` 的 BUILD 列已经打印。缺的是**多目标**：

* 输出尺寸-时间的 Pareto 前沿（`tuned` 可能只比 `thin` 小 3% 却慢 2×）；
* `--time-budget 1.5x`：在"不超过默认 profile N 倍时间"的约束下取最小产物；
* 与 README 已有的 `dist` profile 主张一致：`release` 快、`dist` 小，选择权交给测量。

---

## 6. 复现命令速查

```sh
# 只读预览全机可回收量（du 口径 + 虚高倍数）
python3 ~/syncfolder/project/dsfolder/scripts/rust-target-audit.py status
python3 ~/syncfolder/project/dsfolder/scripts/rust-target-audit.py toolchains   # ~/.rustup 待核清单

# 工作集健康度
cd <repo> && cargo build --release && cargo build --release   # 第二次应 ~0.01s
touch src/main.rs && time cargo build

# 指纹为什么脏
CARGO_LOG=cargo::core::compiler::fingerprint=info cargo build -vv 2>&1 | grep -i dirty

# sccache 命中率（注意：同 target dir 重编才有命中，见 §4.2）
sccache --zero-stats
rm -rf /tmp/rs-a && CARGO_TARGET_DIR=/tmp/rs-a RUSTC_WRAPPER=sccache cargo build --release
sccache --show-stats | head -12
rm -rf /tmp/rs-a && CARGO_TARGET_DIR=/tmp/rs-a RUSTC_WRAPPER=sccache cargo build --release   # 应 100% 命中
rm -rf /tmp/rs-b && CARGO_TARGET_DIR=/tmp/rs-b RUSTC_WRAPPER=sccache cargo build --release   # 应 0% 命中

# du vs 逻辑（硬链接核算）
python3 - <<'EOF'
import os, collections
root = os.environ.get('TARGET', 'target')
agg = collections.defaultdict(lambda: [0, 0, set()])
for dp, _, fn in os.walk(root):
    for f in fn:
        e = os.path.splitext(f)[1]
        p = os.path.join(dp, f)
        try: st = os.lstat(p)
        except OSError: continue
        a = agg[e]; a[0] += st.st_size; a[1] += st.st_blocks*512; a[2].add((st.st_dev, st.st_ino))
for e, (size, blocks, inodes) in sorted(agg.items(), key=lambda kv: -kv[1][1])[:8]:
    infl = size/blocks if blocks else 0
    print(f"{e or '(no ext)':12} paths/inodes={inodes and len(inodes):>6}  logical={size/1048576:9.1f}MB  du={blocks/1048576:9.1f}MB  inflation={infl:.2f}x")
EOF
```

---

## 附录 A：本次测量原始数据

* target 目录：14 个，`~/syncfolder/project` 下合计约 26.5 GB（明细见 §1.1）。
* `unirun/target`：du 1.98 GB / 逻辑 2.18 GB / 19 053 文件 / 12 043 inode；`debug/deps` 1.8 GB（14 875 文件）、`debug/build` 125 MB、`release` 72 MB、`package` 644 KB。
* `unirun` debug/deps 分类型：`.o` 13 866 路径 / 6 856 inode / 逻辑 731.9 MB / du 516.0 MB（1.42×）；`.rlib` 247 / 620.8 MB / 621.2 MB；`.rmeta` 518 / 360.9 MB / 361.7 MB；`.dylib` 23 / 80.4 MB / 80.5 MB。
* 全树 rlib：2 071 个 / 4.05 GB 逻辑（`syn` 42 份、`quote` 30、`serde` 28）。
* `~/.rustup`：10.34 GB；审计工具判定 5.85 GB 无任何 `rust-toolchain.toml` 引用。
* `~/.cargo`：2.4 GB（`registry/src` 1.9 GB / 1 497 项、`cache` 263 MB、`index` 68 MB sparse、`advisory-dbs` 45 MB、`bin` 138 MB）。
* sccache：本地缓存 331 MB；`43 请求 / 0 执行`；不可缓存原因 `incremental 20 / missing input 12 / crate-type 5`。控制实验见 §4.2。
* `~/.cache/rustopt/work`：415 MB（2 个目录）。
* 磁盘：`/System/Volumes/Data` 460 GiB，已用 339 GiB，可用 93 GiB（79%）。

### 一句话回滚路径

| 被删的东西 | 怎么回来 |
|---|---|
| `target/**/incremental`、被取代 hash、`flycheck0`、`tmp` | `cargo build` |
| 交叉 triple 残留 | `cargo build --target <triple>` |
| `~/.cargo/registry/src` | 从 `registry/cache/*.crate` 自动重新解包（必要时联网） |
| sccache 本地缓存 | 重新编译自然回填（当前命中率 0，无损失） |
| `~/.cache/rustopt/work` | 下一次 `rustopt plan`（代价：一次全变体冷编） |
| rustup 工具链 | `rustup toolchain install <version>`（需联网） |
