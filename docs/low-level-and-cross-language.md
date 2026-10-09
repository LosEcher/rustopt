# 更底层的方案 / 可参考的开源实现 / 其他语言的方案

配套文档：[compile-speed-disk-cache.md](compile-speed-disk-cache.md)（上一层：profile、清理、sccache 实测）、[bottleneck-verified.md](bottleneck-verified.md)（瓶颈实测与优化验证：机制链路、关键路径、5 臂冷编对照、sccache 双向验证）
本文标注约定：**【实测】**＝我在本机跑出来的数字或行为；**【调研】**＝外部资料，未在本机验证；**【待核】**＝我知道不确定性，不要当结论用。

---

## 0. 三层视角：杠杆到底在哪一层

| 层 | 你能动的东西 | 本机可用性 | 收益性质 |
|---|---|---|---|
| **L1 项目配置** | `[profile.*]`、feature、workspace 结构、`.cargo/config.toml` | ✅ 全可用 | 每个项目各调一遍，收益 5–40% |
| **L2 工具链机制** | cargo 的 unit hash / `build-dir` / 指纹；rustc 的增量、并行前端、pass 级开关 | ⚠️ 多半在 nightly | 一次配置，全项目受益 |
| **L3 生态与系统** | 内容寻址缓存、共享 target dir、分布式执行、文件系统/同步目录 | 部分可用（本机无 bazel/nix/ccache） | 收益最大，但要求"版本与路径一致"这个前提 |

**本文最重要的结论（【实测】）**：L3 的缓存手段在你机器上收益有限，**根因不在缓存工具，而在 24 个独立 `Cargo.lock` 造成的版本分叉**（1,484 条冗余版本分支）。任何缓存都无法复用"不同版本"的 crate。**先对齐版本，再谈缓存。**

---

## 1. 更底层（一）：nightly 里已经存在的机制

本机 `rustc +nightly` = **1.100.0-nightly**（已安装）。下面是我在本机用 `rustc +nightly -Z help` / `cargo +nightly -Z help` **实读**到的相关开关（不是文档抄录）。

### rustc 侧

| flag | 作用 | 备注 |
|---|---|---|
| `-Z cache-proc-macros=yes` | **缓存 derive 过程宏展开结果** | 帮助面：derive 密集的 crate；help 里自带 **"(potentially unsound!)"**，默认 `no` |
| `-Z share-generics=yes` | 让当前 crate 共享其泛型实例化 | 与 `-C prefer-dynamic`/dylib 组合才有意义；默认关闭 |
| `-Z incremental-info=yes` | 打印增量复用（或未复用）的高层信息 | 诊断"为什么没复用"的第一手工具 |
| `-Z assert-incr-state=loaded\|not-loaded` | 断言增量缓存是否被加载 | 用来证明"缓存真的生效" |
| `-Z disable-incr-comp-backend-caching` | 关闭 codegen 后端的对象缓存 | 反向开关，用于定位问题 |
| `-Z incremental-verify-ich` | 校验增量 ICH 的额外属性 | 排查"增量结果不可信" |
| `-Z self-profile` + `-Z self-profile-events=…` | 自剖析；事件里可开 `query-cache-hit`、`incr-cache-load`、`incr-result-hashing`、`artifact-sizes` | 比 cargo `--timings` 更细（到 query 级） |
| `-Z time-passes=yes -Z time-passes-format=json` | **逐 pass 计时，且可输出 JSON** | 我用它做了下面的归因；`link`/`run_linker` 也在其中 |
| `--jobs-frontend=N` / `--jobs-backend=N` | 并行前端 / 后端（取代 `-Zthreads`、`-Z no-parallel-backend`） | 前端默认仍是 1 线程【调研】 |
| `-Z hint-mostly-unused` | 提示"本 crate 大部分不会被用到" | 对巨型 API 依赖有效，广泛使用会变慢【调研】 |
| `-Z meta-stats=yes` | crate metadata 统计 | 对应 `generate_crate_metadata` 的占比 |

### cargo 侧

| flag | 作用 | 本机行为 |
|---|---|---|
| `-Z checksum-freshness` | 用校验和而非 mtime 判断新鲜度 | 未实测 |
| `-Z gc` | "Track cache usage and garbage collect unused files" | **【实测】在 1.100-nightly 上未观察到任何行为**：开 `-Zgc` 构建两次（探针目录 + `~/.cargo` 顶层）都没有出现任何 usage/gc/跟踪状态文件，也没有回收动作。**现在不要把它当清理方案**（可能要与后续 layout/版本配合，【待核】）。 |
| `-Z build-dir-new-layout` | 新的 build dir 布局 | **【实测】有效，而且是清理策略的好消息**：配合 `build.build-dir`（`CARGO_BUILD_BUILD_DIR`）后，`target/debug/` 只剩**最终产物**（如 `target/debug/gcprobe3` + `.d`，**没有 `deps/`**），而 `incremental/`、`build/<pkg>/<hash>/out/*.o`、build script 的 `OUT_DIR` 全部落到 **`build/debug/`**。⚠️ **`build-dir` 路径相对 workspace 根解析**（我第一次把 `CARGO_TARGET_DIR` 指到 `/tmp` 却看到仓库根冒出 `build/`，就是这个原因）。含义：**产物与中间物分家后，"回收粒度"终于可以按目录划**；但既有清理工具（`cargo-sweep`、`rust-target-audit.py`、`cargo-orphan-gc`）都按旧布局找路径，切换布局前要确认它们的适配 |
| `-Z fine-grain-locking` | 细粒度锁替代整仓锁 | 未实测；对"多进程并发构建"有意义 |
| `-Z embed-metadata` | 不在库产物里嵌入 metadata | 与"产物更小、更可共享"相关（`generate_crate_metadata` 在本机占 2.3%） |
| `-Z binary-dep-depinfo` | 跟踪依赖产物的变化 | 诊断"改了依赖产物但不重编" |
| `-Z section-timings` | `--timings` 的分段计时 | 未实测 |
| `-Z no-index-update` | 不刷新 registry index | 离线/CI 提速 |

上游方向（**【调研】**，来自 2026 年的目标与博客，链接见文末）：cargo 有一个 **cross-workspace cache** 的 2026 目标，以及已落地的 **build-dir v2**（把中间产物与最终产物分开，为"跨 workspace 缓存、自动清理陈旧单元、细粒度锁"铺路）。这正是把"共享 target dir"从民间偏方变成官方能力的路线。

### rustc 时间花在哪：本机逐 pass 归因【实测】

`rustopt` 全新 target dir，nightly，dev，`-Ztime-passes=format=json`：
累计 26,552 个 pass 事件、**20.00 s 各 unit 计时之和**（并行执行，实际墙钟约 5 s；包含父节点 `total`，**仅用于相对比较**）。

| pass | 累计 | 占比 | 含义 |
|---|---|---|---|
| MIR_borrow_checking | 1.909 s | 9.5% | 借用检查（前端，无法从外部绕开） |
| type_check_crate | 1.526 s | 7.6% | 类型检查 |
| codegen_crate | 0.963 s | 4.8% | codegen |
| LLVM_passes | 0.890 s | 4.4% | LLVM 优化 |
| `link` / `link_crate` / `link_binary`（嵌套） | 0.77 / 0.77 / 0.77 s | 各 ~3.8% | 链接 |
| `run_linker` | 0.601 s | 3.0% | 真正调用链接器 |
| expand_crate / macro_expand_crate | 0.525 s | 2.6% | 宏展开（`-Zcache-proc-macros` 的靶子） |
| monomorphization_collector_graph_walk | 0.522 s | 2.6% | 单态化收集 |
| generate_crate_metadata | 0.460 s | 2.3% | metadata 生成（`-Zembed-metadata`） |

读法：**前端（typeck+borrowck）≈17% 是最大块，且外部工具无能为力**；codegen+LLVM ≈9%；链接在**这个小仓**上约 4%（大二进制会远高于此）；宏展开 2.6%。

### 怎么用这些开关（三条可操作路径）

```sh
# ① 证明"增量真的生效了"，而不是猜
RUSTFLAGS="-Zincremental-info=yes" cargo +nightly build 2>&1 | tail -40

# ② 定位时间到 pass 级（含 link / run_linker）
rm -rf /tmp/tp && CARGO_TARGET_DIR=/tmp/tp \
  RUSTFLAGS="-Ztime-passes=yes -Ztime-passes-format=json" cargo +nightly build 2>/tmp/tp.err

# ③ derive 密集项目的实验性开关（help 自称 potentially unsound，仅实验）
RUSTFLAGS="-Zcache-proc-macros=yes" cargo +nightly build
```

---

## 2. 更底层（二）：unit hash —— 为什么"共享缓存"在本机只兑现了一半【实测】

### 2.1 实验

把两个真实项目编到**同一个** `CARGO_TARGET_DIR`（dev，cargo 1.97 stable）：

| 步骤 | 墙钟 | 编译 crate 数 |
|---|---|---|
| `run-diff` 独占全新目录（基线） | 5.20 s | 22 |
| `rustopt` 编入共享目录 | 3.67 s | 12 |
| **`run-diff` 编入同一共享目录** | **3.18 s** | **17** |
| `run-diff` 独占另一个全新目录（对照） | 3.57 s | 22 |

* 共享目录让第二个项目 **少编 5 个 crate（22→17）**，墙钟比对照 **−11%**；
* 两个项目在同一个 target dir 里的 rlib 数量：共享目录 25 个，独占目录 20 个 ⇒ **共享目录里出现了同一 crate 的两份 unit**。

### 2.2 复用了哪些、没复用哪些

| 结果 | crate |
|---|---|
| **成功复用（1 份）** | `itoa`、`memchr`、`serde_core`、`serde_json`、`zmij` |
| **各编两份** | `serde`、`serde_derive`、`syn`、`quote`、`proc-macro2`、`unicode_ident` |

### 2.3 根因：不是 feature，是**版本**

先用 `-v` 对比 `serde` 的 rustc 调用行，差异只有 `-C metadata` 与 `-C extra-filename`（`89360265db301f2e` vs `89c6b60ae2218f93`），**这不是直接输入，而是从依赖 Merkle 式传播上来的**。
继续追到**叶子 crate**（无任何依赖）`unicode_ident`：

```
- .../unicode-ident-1.0.26/src/lib.rs
+ .../unicode-ident-1.0.24/src/lib.rs
```

**两个项目各自的 `Cargo.lock` 把同一个叶子 crate 锁到了不同版本**，于是 `proc-macro2 → quote/syn → serde_derive → serde` 整条链的 `-C metadata` 全部分叉 ⇒ 6 个 crate 各编两份。feature 集合我用 `cargo tree -e features -i serde` 核对过，**完全一致**。

### 2.4 全机量化：版本分叉是本机最大的"隐形重复源"

扫描 `~/syncfolder/project` 下 **24 个 `Cargo.lock`**（含 `src-tauri/`）：

| 指标 | 数值 |
|---|---|
| `(crate, version)` 记录 | **8,723** |
| 去重后的 crate 名称 | **2,006** |
| **出现多个版本的 crate** | **669** |
| **冗余版本分支**（同 crate 多版本，各自都要单独编译） | **1,484** |

版本分支最多的：`syn` **12** 个版本、`rand` 12、`bitflags` 11、`cc` 11、`wasm-bindgen` 族各 10、`zerocopy` 10、`libc` 8、`tokio` 6、`serde_json` 6。
（`syn`：3.0.3 ×7 / 2.0.117 ×5 / 3.0.6 ×2 / … / 1.0.107，跨 22 个项目。）

**这就是 4.05 GB rlib 重复与"`syn` 42 份"的根因。**

### 2.5 结论与动作

> **对齐版本 = 最底层的缓存复用**。缓存工具（sccache、共享 target dir、Bazel、Nix）对"不同版本"一律无能为力。

按收益排序：

1. **把互相依赖/共用依赖的仓纳入一个 workspace**（一个 lockfile ⇒ 一套版本）。dsfolder 下 7 个仓依赖高度重叠（serde/serde_json/sha2/libc/toml/ureq），是最合适的候选。
   **但先看两个实测障碍**：① 7 个仓现在都把 `[profile.*]` 写在自己的 `Cargo.toml` 里，而**合并后成员 profile 会被忽略并告警**（本机用最小复现验证：`warning: profiles for the non root package will be ignored, specify profiles at the workspace root`）⇒ profile 必须上移到根；② **`session-index` 的 `[profile.release]` 是 `opt-level = "z"`，其余 6 个刻意留空**（注释写着 "Intentionally empty: cargo's defaults. The size knobs live in `[profile.dist]`"）⇒ 一刀切合并要么让 6 个仓静默变慢，要么让 `session-index` 失去设定。7 个仓都是 edition 2021，`rust-version` 分别为 1.87 / 1.88（其余未声明）。
   **所以：合并只做在 profile 政策一致的子集上**（那 6 个是一致的；`session-index` 单独留），或先把 profile 政策统一到根再合并。
2. 不能合 workspace 时：**统一 lockfile 策略**（同一批依赖用 `cargo update -p <crate>` 一起对齐），并定期 `cargo tree -d` 检查；
3. 用 `cargo-hakari`/workspace-hack **钉死 feature 集合**（本机已证明 feature 一致但版本不一致照样不复用；反过来版本一致但 feature 不一致也不复用）；
4. 只有做完 1–3，共享 target dir / sccache 才谈得上收益。

---

## 3. 更底层（三）：系统层

### 3.1 根机制是"内容寻址的 action cache"

Go 的 `GOCACHE`、Bazel/Buck2 的 action cache + CAS、Nix 的 store、sccache 的本地/远程缓存，本质是同一件事：**用"输入内容的哈希"当 key，把输出存进去**。Rust 生态在这件事上是落后的：cargo 的 target 目录是"按 unit 身份 + mtime 新鲜度"的**可变工作区**，不是 CAS。因此：

* 想跨项目/跨机器复用 ⇒ 只能靠外挂（sccache）或把整个 target 目录当黑盒搬运（前提是路径一致）；
* cargo 上游的 cross-workspace cache 目标正是在补这一课。

### 3.2 共享 target dir：机制、实测收益、代价

* **机制**：同一个 `CARGO_TARGET_DIR` 下，cargo 为每个 unit 建一次产物；unit hash 相同的直接复用。**实测**：22→17 crate、−11% 墙钟（§2.1）。
* **代价 1**：feature/版本不一致时目录里会出现多份同一 crate（实测 25 vs 20 个 rlib），**目录只增不减**（`-Zgc` 目前不可用）。
* **代价 2**：跨项目并发构建会争抢整仓锁（`-Z fine-grain-locking` 是上游的应对）。
* **结论**：只对"同版本 + 同 feature"的项目组有意义 —— 也就是第 2 节的那套前提。

### 3.3 文件系统层

| 手段 | 本机情况 | 判断 |
|---|---|---|
| **硬链接农场** | cargo 自己就在用：`unirun` 的 `.o` 13,866 路径 → **6,856 inode**（逻辑 731.9 MB / 实占 516 MB） | 这就是"删了不一定省"的原因；别再用逻辑字节做决策 |
| **APFS clonefile（COW）** | **【实测】`cp -c` 可用**（macOS APFS 原生 reflink）；但 `du` **看不到共享**（两个文件都报全额占用） | 没有现成工具对 `target/` 做 reflink 去重；`fclones`/`rmlint`/`jdupes` 本机未装，且它们做硬链接而非 reflink。**理论可行、工程不划算** |
| **tmpfs / RAM disk 当 target** | 未测 | 26.5 GB 放不进内存；只对"小仓 + 海量小文件"有边际收益，先测再谈 |
| **overlay/union** | 未用 | Linux/容器场景；macOS 无原生 overlayfs |

### 3.4 一个容易被忽略的 IO 因素：项目在云同步目录里【实测】

* `~/syncfolder/project` **确实在坚果云（Nutstore）的同步 sandbox 内**：`~/.nutstore/logs/client.log`（29,278 行）里出现 `~/syncfolder/project/…` 的同步记录（含 "record sync error … SymlinkOrAlias" 与服务端删除传播）。
* **但 `target/` 看起来已被排除**：`dsfolder/rustopt/target`、`cankey/target`、`canpad/target` 都带 `com.apple.fileprovider.ignore#P`，所有 target 目录还带 `com.apple.metadata:com_apple_backup_excludeItem`；且 29,278 行日志里 **`/target/` 出现 0 次**。
* 结论：**目前没有被同步在churn**（这是好消息，但保护来自同步客户端的忽略规则，不是你的配置）。**零风险做法**：把 `CARGO_TARGET_DIR` 指到同步树之外（例如 `~/.cache/cargo-target/<project>`），或在坚果云里显式排除 `target/`。11 GB 的 `cantool/src-tauri/target` 没带 fileprovider 标记，值得单独确认。

### 3.5 分布式与远程执行

`sccache-dist`、Bazel RBE、Buck2 remote execution、`icecc`/`distcc`（本机都没装）。判断：**对 dsfolder 这种单机小仓不值得**——收益要到"多机器 + 大量 C++/Rust 重编译"才成立，且要求可重定位的构建（hermetic），而 Rust 的 `CARGO_MANIFEST_DIR`/绝对路径恰恰是障碍（见上一篇 §4.2 的 sccache 实测）。容器场景里更便宜的答案是 **BuildKit cache mount**（`--mount=type=cache,target=/app/target`）。

---

## 4. 可参考的开源实现：读什么、偷什么

> 本节的"机制"描述来自公开文档；标注 **【待核】** 的部分我未在本机或原始源码上确认。

| 实现 | 机制 | 值得偷的点 |
|---|---|---|
| **ccache** | 直接模式/预处理模式 + manifest；`base_dir`/`CCACHE_BASEDIR`（**单数**；复数 `SCCACHE_BASEDIRS` 是 sccache 的）让**绝对路径归一化**，`CCACHE_SLOPPINESS` 放宽不可哈希的输入 | **路径归一化**是跨 checkout 复用的前提。Rust 侧对应物是 `SCCACHE_BASEDIRS`，但**本机实测救不了** `CARGO_TARGET_DIR` 进 key 的问题（上一篇 §4.2） |
| **cargo-orphan-gc** | 编译器 wrapper：只在"同一 build family 成功重编"后回收被取代的一代，并收集 rustc 自己没删掉的 surplus 增量会话；`inner-wrapper` 链式兼容 sccache | **不变式表本身就是设计清单**（无成功替换不回收 / 未知所有权泄漏 / 活跃输入加租约 / fail-closed / dry-run 默认真）。也示范了 wrapper 链的正确顺序（naive 的 `rustc-workspace-wrapper` 嵌套会**静默让 sccache 失效**）。代价：改指纹、每次调用走 out-dir、`-Zfine-grain-locking` 不兼容 |
| **Dune (OCaml) 的 cache trim** | `dune cache trim --size=BYTES`，把 **link count > 1** 的条目判定为"不可回收开销" | **du/inode 纪律的现成规则**：硬链接数 > 1 的条目删了不释放空间（正是本机 `.o` 13,866 路径 / 6,856 inode 的情形）。另：因"用户规则可能含未声明依赖"而默认不用用户规则 ⇒ 与 `RUSTFLAGS` 漂移同类 |
| **sccache** | 本地磁盘缓存 + 远端后端（S3/GCS/Azure/Redis/WebDAV/GHA）+ `sccache-dist` 分布式 | 单文件实现的"多后端缓存层"抽象；以及它的反面教材：**key 里混入了 `CARGO_*` 环境变量**，导致路径一变命中率归零 |
| **Go `cmd/go/internal/cache`** | **内容寻址的 action id**：`(compiler, flags, inputs)` 的哈希即 key；`GOCACHE` 跨项目/跨 checkout 天然共享 | Rust 最该学的一个。Go 用"放弃增量编译"换来了"缓存天然可共享"——一个明确的取舍 |
| **Bazel / Buck2** | action cache（按 action 输入哈希）+ CAS（内容寻址存储）+ 远程执行；**hermetic action** 是前提 | "把构建动作变成纯函数"这个前提条件；以及"缓存命中率是可观测指标"这件事 |
| **Nix** | 全局 store + 内容寻址派生（CA derivations） | 最强版本的"缓存即真值"；也说明为什么**事后外挂缓存**永远追不上**架构上就内容寻址**的系统 |
| **Gradle build cache / Turborepo / Nx** | 任务级内容哈希 + 本地/远程缓存 + 输出**重定位**（relocatability） | 任务粒度（而不是文件粒度）的缓存；"输出重定位"解决路径问题 |
| **Dune (OCaml) / Zig** | 依赖图驱动的增量 + 内容哈希的构建缓存 | 小工具也能有正确的内容寻址缓存：**先定义"输入集合"，再谈缓存** |
| **mold / wild / sold** | 现代链接器（并行、ICF 相同代码折叠） | macOS 不可用（上一篇 §2.2），但代码是"链接为什么慢"的最佳教材；**ICF 顺带省产物尺寸** |
| **cargo-sweep / kondo / cargo-cache / `rust-target-audit.py`（本机已有）** | target 与 `~/.cargo` 的清理 | 已有细档工具（du 口径 + 年龄门），不必重造；`cargo-sweep` 的缺口是**不动 `incremental/`、报逻辑字节** |
| **fclones / jdupes / rmlint** | 文件去重（硬链接/reflink） | 【待核】对 cargo `target/` 的安全性；注意**硬链接不省 inode 的读取成本**，reflink 才是真省块 |
| **cargo-hakari (workspace-hack)** | 生成一个 crate 钉死全 workspace 的 feature 集合 | **和 §2 是同一件事的另一半**：版本对齐 + feature 对齐 |
| **cargo-nextest / cargo-chef / cargo-wizard / cargo-llvm-lines / cargo-bloat / rustc-perf / hyperfine** | 测试运行、Docker 层缓存、profile 预设、单态化统计、体积归因、编译器基准 | `cargo-chef` 是"容器里只重编源码层"的标准答案；`cargo-llvm-lines` 找单态化爆炸；`rustc-perf` 是上游的度量口径 |

**如果要给一个 Rust 产物缓存"偷设计"**：拿 Go 的 *action-id 内容寻址*当骨架，拿 Gradle/Turbo 的 *任务级 + 输出重定位*当粒度，拿 ccache 的 *路径归一化*当兼容层，拿 Bazel 的 *hermetic 前提*当准入条件，拿 Nix 的 *store 语义*当正确性上限。

---

## 5. 其他语言的方案

> 除注明外均为 **【调研】**（外部资料，未在本机验证）。

| 语言 | 核心加速设计 | 缓存/复用粒度 | 分布式 | 移植到 Rust？ |
|---|---|---|---|---|
| **C/C++** | 头文件模型是根成本 ⇒ PCH、unity/jumbo build、C++20 modules；链接器（mold/lld/wild） | 目标文件级 + `ccache` 的 action 级 | 成熟（distcc/icecc/Incredibuild/FASTBuild） | **部分**：Rust 没有头文件可预编译；但"统一编译单元数量"（unity build ↔ crate 粒度）与链接器思路可借鉴 |
| **Go** | 无头文件、包粒度、单趟编译；**GOCACHE 内容寻址**跨项目免费共享 | action 级（编译器+flags+输入哈希） | 无（不需要） | **最值得学**：内容寻址 + "不做增量编译"的取舍。Rust 因单态化/增量而无法照搬 |
| **Java/Kotlin** | 构建期几乎不做 codegen（JIT 承担）；Gradle **增量编译 + 构建缓存 + 远程构建缓存**；增量注解处理 | 任务/类级，且**输出可重定位** | Gradle remote build cache | **部分**：任务级缓存的思想；AOT/native-image 反而更慢 |
| **JS/TS** | esbuild（Go 实现、并行、**不做类型检查**）；SWC；Turbopack；`tsc --incremental`/project references | 文件/模块级 | Nx/Turborepo 的远程缓存 | **部分**：**"类型检查与转换分离"** 对 Rust 无直接对应（rustc 一体），但"把慢的部分拆到另一个进程/目标目录"可借鉴（`cargo check`） |
| **Swift/ObjC** | batch mode vs whole-module optimization；**explicit modules**、Clang modules；`swift-driver` | 模块级 | 有（distributed builds）【待核】 | "explicit modules"是把隐式依赖变显式以换取增量——与 Rust 的 `.rmeta`/接口哈希同构 |
| **Haskell / OCaml** | `.hi` 接口文件、`-fno-code`；Dune 的依赖图缓存 | 模块接口级 | 少 | `.hi` 与 Rust `.rmeta` 同构；**"改类型签名就重编世界"是两者共同的病理** |
| **Zig** | 单一二进制、`zig build` 的**内容哈希缓存**、`zig cc` 当交叉编译器 | 内容哈希 | 少 | **思想可借鉴**：小工具也可以有正确的内容寻址缓存 |
| **C#/.NET (Roslyn)** | **编译器服务器**（复用进程与已加载的引用）、增量 source generator、MSBuild up-to-date 检查 | 项目/生成器级 | 少 | **进程常驻**这一条 Rust 侧只有 `rust-analyzer`/`sccache` 部分覆盖；cargo 无守护进程 |

### Rust 结构上做不到的（别浪费时间）

1. **没有头文件可预编译** ⇒ C++ 的 PCH 路线不存在；对应物是"预构建依赖产物"（≈ 共享 target dir / hakari）。
2. **单态化按 crate 边界重复发生** ⇒ 跨 crate 的泛型会各编一份；这是 `-Zshare-generics`、`cargo-llvm-lines`、`cargo bloat` 存在的理由，只能压不能消。
3. **没有稳定 ABI** ⇒ 无法像 C++/JVM 那样用动态库把"重链接"成本切掉；`-C prefer-dynamic`/dylib 是可选但会改变发布形态。
4. **构建期就做完 codegen**（不像 JVM 把优化推给运行时）⇒ 编译时间必然更高，这是设计取舍而不是缺陷。

### 可移植的 8 条机制（Rust 现状）

| 机制 | 出处 | Rust 现状 |
|---|---|---|
| 内容寻址 action cache | Go / Bazel / Nix / sccache | 只能外挂（sccache）；cargo 有 cross-workspace cache 目标【调研】 |
| 任务级缓存 + 输出重定位 | Gradle / Turbo / Nx | 无；cargo 的产物里含绝对路径 |
| 路径归一化 | ccache `BASEDIRS` | sccache 有，但**本机实测无效**（env 进 key） |
| holy 前提：hermetic action | Bazel | Rust 不 hermetic（`CARGO_MANIFEST_DIR`/`OUT_DIR` 进编译） |
| 编译单元粒度调整（unity build） | C++ | 对应"crate 数量/大小"，可直接做 |
| 类型检查与 codegen 分离 | TS / Java | 部分：`cargo check` 与独立 target dir |
| 分布式 action 执行 | Bazel RBE / sccache-dist | 有但门槛高；小仓不值得 |
| 相同代码折叠（ICF） | mold / lld | 链接器选项，macOS 不可用 |

---

## 6. 本机该做什么（结论）

按"收益 ÷ 成本"排序：

1. **对齐版本（最高优先）**：把 7 个 dsfolder 仓（依赖高度重叠）并入一个 workspace，或至少统一 lockfile 策略。这是 1,484 条冗余版本分支与 4.05 GB rlib 重复的直接解药，也是任何缓存生效的前提。
2. **用 nightly 做诊断，而不是赌收益**：`-Zincremental-info` 证明增量是否生效、`-Ztime-passes=format=json` 定位到 pass、`-Zself-profile-events=query-cache-hit` 看 query 级缓存命中。`-Zcache-proc-macros` 只作实验（自带 unsound 警告）。
3. **共享 target dir 只在"同版本 + 同 feature"的项目组内用**，并配 `cargo-hakari` 钉 feature；否则目录只增不减，而且**没有可用的自动回收**（`-Zgc` 本机实测无行为）。
4. **确认同步目录边界**：target 目前已被坚果云忽略（xattr + 日志双证据），但建议把 `CARGO_TARGET_DIR` 移出同步树，做到"不依赖客户端的忽略规则"。
5. **不要现在投入**：分布式执行、reflink 去重工具、cargo `-Zgc`（【实测】本机 1.100-nightly 无可观察行为）。
6. **若坚持"保留 incremental 又要回收"**：目前唯一的现成实现是 `cargo-orphan-gc`（评估见配套文档 §5.2：crates.io 真实存在但仅 38 次下载、会改 cargo 指纹各一次全量重编、必须先用 shadow 模式、与 `-Zfine-grain-locking` 不兼容）。本机更简单的路径仍是给那 6 个再生 incremental 的仓加 `incremental = false`——**把问题消掉，而不是管理它**。

---

## 附：本文引用的外部资料

* rustc 的 `-Z` 选项（本文清单为**本机 `rustc +nightly -Z help` 实读**）：<https://doc.rust-lang.org/nightly/rustc/codegen-options/index.html>
* cargo 不稳定特性（`-Zgc`、`-Zchecksum-freshness`、`build-dir` 等）：<https://doc.rust-lang.org/nightly/cargo/reference/unstable.html>
* cargo cross-workspace cache 目标（2026）：<https://goals.rust-lang.org/2026/cargo-cross-workspace-cache.html>
* build-dir v2 试行公告：<https://blog.rust-lang.org/2026/03/13/call-for-testing-build-dir-layout-v2/>
* rustc 并行前端目标：<https://goals.rust-lang.org/2026/parallel-front-end.html>
* cargo 指纹调试：`CARGO_LOG=cargo::core::compiler::fingerprint=info`
* cargo 文档：profiles / config / resolver / timings / build-scripts（<https://doc.rust-lang.org/cargo/reference/>）
* sccache 的 Rust 限制与 `SCCACHE_BASEDIRS`：【调研】sccache 文档 `docs/Rust.md`
* Go 构建缓存设计：`cmd/go/internal/cache` 与官方文档（【调研】）
* 其余实现（Bazel/Nix/Gradle/Turbo/Dune/Zig/Roslyn）为公开文档综述【调研】，**未在本机验证**
