# rustopt 自优化评审（CBM / KG / hotpath）

2026-10-07 · Apple M1 Pro（8P+2E / 10 逻辑核）· rustc 1.97.0 · macOS
方法：codebase-memory-mcp（CBM，符号级知识图谱）+ hotpath 0.28（函数级时间/内存剖析）+ 真实输入 A/B 对照。

**状态**：§2/§3/§4/§6（部分）/§7/§8 与两项微优化**已落地到本仓**（61 个测试、`clippy -D warnings`、`fmt --check` 全绿）；
§5（变体并行）与 §6 的流式改造**留待后续迭代**，触发条件与所需数据见 §12。原先的 `docs/evidence/patches/*.diff` 已应用，故删除，改动见工作区 `git diff`。

配套：[FINDINGS.md](FINDINGS.md)（构建速度/磁盘的调查台账）、[BASELINE-project-sizes.md](BASELINE-project-sizes.md)。

---

## 0. 结论表

| # | 可优化项 | 类别 | 实测收益 | 状态 |
|---|---|---|---|---|
| 1 | `guard` 三遍扫描 → 单遍 + 预过滤 + 字节化 `strip_noncode` | CPU/IO | 7.5×（718 文件 / 7.85 MB），并修正证据行号 off-by-one | **已修复** |
| 2 | `dir_size` 每项两次 stat，且跟随符号链接 | 系统调用/正确性 | −30%（415 MB / 1843 文件），字节总数不变 | **已修复** |
| 3 | `plan`/`check` 前置三个探测串行 → 并行 | 墙钟 | 1.82×（49.4 → 27.2 ms，5 次中位数） | **已修复** |
| 4 | 变体矩阵串行构建 → 并行（需配 `-j` 上限） | 墙钟 | 2.24×（34.35 s → 15.36 s，冷编 5 变体） | 待数据（§12） |
| 5 | cargo JSON 流：`Value`/行 + 整块拷贝 → 类型化 + 预过滤 | CPU/内存 | 2.4×（6.05 MB / 5400 行：12.17 → 5.07 ms） | **已修复**（流式化待数据） |
| 6 | `.crate` 打包把 `docs/` 全带上 | 发布物体积/信息卫生 | 126.1 → 40.4 KiB 压缩后（−68%） | **已修复** |
| 7 | `check` 只能量 `--release`，量不到真正发布的 `--profile dist` | 门禁语义 | rustopt 自身：以前 `check` 报 940,720 B，实际发布物 571,536 B | **已修复**（`--build-profile`） |
| 8 | ledger 逐事件开/关文件、无上限、用 `Value` 重解析 | 小 | 单次 plan = 2+2N 次 open；`runs.jsonl` 无 prune | 待数据（§12） |
| 9 | `--variants` 只有 8 个预置，无 `--config` 透传 | 功能缺口 | 无法测"已有 profile + 一个额外 knob" | 待判断（§12） |
| 10 | `Args::parse` 逐项 `clone`；`guard::walk` 双重 stat | 微 | 量级 <1 ms | **已修复** |

**不建议再投入的**：依赖收敛（只有 2 个直接依赖，无死代码，`clippy -D warnings` 干净）；`opt-level` 调参（实测 `z 537712 < s 586496 < 3 669040`，`tuned` 档对它自己就是最优）。

**一笔要认账的成本**：上述修复给二进制加了代码。dist 档 537,712 → **571,536 B（+6.3%）**，release 档 940,720 → **1,024,816 B（+8.9%）**。
一个讲字节的工具在自己的体积上退让了 6%，换来的是 7.5× 的扫描速度与一个"量得对"的门禁——但如果体积预算更重要，这笔账应该由数据来定（§12 第 3 条）。

---

## 1. 仪器与图谱（先说"热路径在哪"）

### 1.1 CBM 图谱

```
index_repository → Users-echerlos-syncfolder-project-dsfolder-rustopt
748 节点 / 1488 边（排除 target、.git、.rustopt）
layers: plan(fan-out 14) → measure(5) → variants；variants / events 是 core（fan-in 9 / 5）
entry_points: src/main.rs::main（唯一）；无未调用符号（除 #[cfg(test)] 测试）
```

结论：模块划分没有问题，没有死代码，没有意外的反向依赖。**优化空间不在结构上，在三个具体实现里**（下节）。

### 1.2 hotpath 函数级归因

`#[cfg_attr(feature = "hotpath", hotpath::measure)]` + `#[hotpath::main]`（feature 关闭时零成本），在 `/tmp/rustopt-hp` 上跑：

`rustopt plan --dry-run`（无构建，纯工具自身开销，未优化版 + debug 构建）：

```
guard::scan       1 call  12.57 ms   ← 其中 first_hit 3 calls 11.78 ms (93.7%)
guard::first_hit  3 calls             ← 同一份文件列表被扫了 3 遍
guard::strip_noncode 39 calls         ← 13 个 .rs × 3 组 marker
```

`39 = 13 × 3` 就是问题本身：**每个源文件被读 3 次、strip 3 次，其中 2/3 是纯冗余。**

release 构建下 `plan --dry-run` 的完整归因（优化前）：

```
冷缓存首次运行：rustopt::cmd_plan 98.8 ms = package_meta 50.5 + toolchain 44.5 + guard::scan 3.65
暖缓存（§4 的 A/B 基线）：rustopt::cmd_plan 49.4 ms
```

即：**一次调用里 95% 的"工具自身时间"是两个互相独立的子进程**，而它们是串行等的。

---

## 2. `guard` 的三遍扫描（已修复）

**问题**（`src/guard.rs`）：`scan()` 对同一个 `rs_files` 调 `first_hit()` 三次（catch_unwind / should_panic / backtrace），`first_hit` 内部对每个文件 `metadata → read_to_string → strip_noncode → 逐行 contains`。`strip_noncode` 还把源码先收集成 `Vec<char>`（**4 字节/源码字节**）再产出 `String`。

**改法**：单遍 `scan_markers(groups)`；读之前先用**原始文本**做预过滤（strip 只会删字节，原文没有的 marker 剥离后也不可能有）；`strip_noncode` 改为按字节走（所有定界符都是 ASCII，UTF-8 续字节 ≥0x80 不会误判）。

**A/B（同一套 bench 二进制，7 次中位数，release）**：

| 输入 | 旧 | 新 | 倍率 |
|---|---|---|---|
| cantool/src-tauri（718 个 .rs，7.85 MB） | 144.71 ms | **19.17 ms** | **7.5×** |
| hotpath-rs（536 个 .rs，4.03 MB） | 81.99 ms | **15.82 ms** | **5.2×** |
| rustopt 自身（13 个 .rs，120 KB） | 2.52 ms | **0.76 ms** | **3.3×** |

**语义对齐**（不只是"findings 数一样"）：差异测试用 `/tmp/guard-torture`（嵌套块注释、raw string、`'é'`、`'\''`、`\` 续行字符串）逐条比 `id|severity|evidence`，三条 finding 完全一致，但行号更正确：

```
真实行号（grep -n）：13 catch_unwind(|| 1) / 15 backtrace::Backtrace / 19 #[should_panic]
旧实现：           12 / 14 / 18      ← 续行字符串把换行吃掉，其后行号整体 −1
新实现：           13 / 15 / 19
```

那是同一处修正：字符串里的 `\` 续行（`"line one \` + 换行）原本不保留换行，导致其后所有证据行号 **−1**。新实现保留它。

落地后：48 个 unit + 13 个真实构建 fixture 的 CLI 测试全绿（`cargo test --locked`），`clippy --all-targets -- -D warnings` 与 `fmt --check` 干净。

---

## 3. `dir_size` 每项两次 stat + 跟随符号链接（已修复）

**问题**（`src/main.rs`）：`p.is_dir()` + `std::fs::metadata(&p)` = 每个 entry 两次 stat；`is_dir()` 会跟随符号链接，work dir 下若有链接环/链接农场会重复计数甚至打转。

**改法**：用 `DirEntry::file_type()`（readdir 自带，零额外 stat，符号链接按链接本身报告）+ `DirEntry::metadata()`。

**实测**（`clean --work-dir ~/.cache/rustopt/work`，415 MB du / 785 目录 / 1843 文件，逻辑 630,079,127 B）：

```
旧：22.0 ms（3 次：22.29 / 21.97 / 27.98）
新：15.5 ms（3 次：15.51 / 15.92 / 20.25）   → −30%
总量口径不变：630,079,127 B（差额 6148 B 是 work 根目录下的 .DS_Store，`cmd_clean` 只对子目录求和，前后都一样）
```

---

## 4. 前置探测串行（已修复）

`plan::run` 里 `package_meta`（cargo 子进程）→ `toolchain`（rustc 子进程）→ `guard::scan`（只读遍历）**顺序执行**，三者互不依赖。用 `std::thread::scope` 重叠即可。

**暖缓存 A/B（各 5 次，`rustopt::cmd_plan` 总时长）**：

```
串行：49.37 52.70 49.44 48.23 49.00 ms   （中位 49.4）
并行：28.04 27.69 27.14 26.89 27.20 ms   （中位 27.2）  → 1.82×
```

内部自洽：并行后总时长 ≈ 最慢的单项（`toolchain` 26.8 ms），串行时 ≈ 三者之和（23.4+26.8+1.4）。
对 `plan --dry-run`（文档里的"快速预览"路径）这是全部耗时；对暖 `check` 约占一半。
落地后的真实二进制（无插桩开销）暖 `plan --dry-run`：**20 ms × 5 次**。

---

## 5. 变体矩阵：冷编 34 s 里的真实结构（待数据）

`plan` 的默认集是 5 次**完整的** `cargo build`（每个变体自己的 `CARGO_TARGET_DIR`），而且 `--config profile.release.*` 对依赖同样生效，所以 5 个变体几乎不共享任何 unit。rustopt 自身冷编：

| 方案 | 墙钟 | 说明 |
|---|---|---|
| 串行（现状） | **34.35 s** | 5 变体顺序 |
| 5 进程并行，不设上限 | 28.92 s | 只 1.19×：每个 cargo 默认 `codegen-units=16`，5×16 在 8 个 P 核上超订 |
| 5 进程并行，各 `-j 2` | **15.36 s** | **2.24×**（10 个 rustc 进程 ≈ 10 逻辑核） |

产物字节完全一致（940720 / 940784 / 936928 / 792944 / 537712，两臂逐项相同）。

**代价与设计建议**：`plan` 把 `duration_ms` 当作推荐的"价格"（`price_ratio_vs_default`）报告，并行会让这些数字变成受争用的上界。因此不要直接默认并行，建议：
1. `--parallel-variants[=N]`：并行跑完矩阵，同时把 `duration_ms` 标为 `timing_contended: true`；
2. 并行结束后**串行重测** `recommended` 与 `default` 两个变体（暖 target dir，实测各约 35 ms），把"价格"恢复成干净数字；
3. `-j` 缺省自动取 `cores / variants`，否则并行反而更慢（上表的 28.92 s 就是反例）。

---

## 6. cargo JSON 流的读法（类型化 + 预过滤已修复，流式待数据）

**问题**（`src/measure.rs`）：`Command::output()` 把 stdout/stderr 全缓冲；`String::from_utf8_lossy(&out.stdout).to_string()` 再整块拷一份；然后**每一行**都 `serde_json::from_str::<Value>`（每行建一张 Map，含 diagnostics 的巨型 rendered 字段也照建）。

**bench 输入**：真实捕获（`cargo build --release --message-format=json`，16,565 B / 25 行，18 个 artifact）按工作区规模放大成 6.05 MB / 5400 行（其中 400 条是 cargo 真实的 `compiler-message` 形状，每条 ~2 KB rendered）。原型 = `"compiler-artifact"` 预过滤 + 借用式 `&str` 类型化反序列化 + 不做整块拷贝：

```
current (Value/line)       median 12.17 ms   497 MB/s
prototype (typed+filter)   median  5.07 ms  1193 MB/s   → 2.4×
```

**内存口径**：现状是 O(cargo 输出)（缓冲 + 拷贝 + 每行 Value）；6 MB 流约多占 12 MB。已落地的部分去掉了"每行 Value"和整块拷贝（就是上表 2.4× 的来源）；剩下的一半——`Stdio::piped()` 逐行读、只留 artifact 行、stderr 用增量 FNV 哈希（只留末 20 行给失败提示）——把内存变成 O(1)，但需要给 stderr 配独立线程（否则管道死锁），**留待有真实大仓的 RSS 数据再动**（§12 第 2 条）。

---

## 7. 打包：一个"讲字节的工具"把自己的调查笔记一起发了（已修复）

`cargo package --list`（**本轮新增本报告与补丁之前**的实测）：49 个文件、**398.8 KiB / 压缩后 126.1 KiB**，其中 23 个文件是 `docs/`（**300 KB，占未压缩载荷 75%**）：
内部中文调查笔记 + `scripts/apply-build-profile.sh` + `docs/evidence/rollback/`（**另外 6 个仓库**的 `Cargo.toml` / `Cargo.lock` 回滚副本）。本轮新增的两份 docs 文件同样落在下面这行的射程内。

加一行：

```toml
exclude = ["docs/", "scripts/", ".github/"]
```

实测：**49 → 24 个文件，398.8 → 145.4 KiB，126.1 → 40.4 KiB（压缩后 −68%）**。
`tests/` 与 `tests/fixtures/` 保留（`cargo package` 拒绝打包含 `Cargo.toml` 的子目录，测试靠 materialize 到临时目录，是有意为之）。
CI 里加了对应门禁（`Packaging content gate`）：`cargo package --list` 一旦列出 `docs/` 就让 job 失败——否则这行 `exclude` 迟早被下一个人删掉。

顺带一个环境坑：`cargo bloat --profile dist` 会**重建一个未 strip 的**二进制覆盖 `target/dist/rustopt`（它自己报的 "file size 803.1KiB" 就是那个旧版未 strip 文件）。发布 job 里若在 stage 之前跑 cargo bloat，会把未 strip 的产物发出去；`.text` 口径不受影响（355.1 KiB 中 std 75.4% / rustopt 14.8% / serde_json 5.6%）。

---

## 8. `check` 量不到真正发布的产物（已修复：`--build-profile`）

`measure::cargo_argv` 原本把 `build --release` 写死，所以 `check` 只能量 cargo 的 `release`。rustopt 自己发布的是 `--profile dist`：

```
修复前
$ stat -f %z target/dist/rustopt        → 537712      （CI 真正上传的资产）
$ rustopt check --manifest . --budget 600KB --no-log
  measured  940.72 KB (940720 bytes)   ← release 档，不是发布物
  verdict   FAIL (over budget by 340.72 KB)   exit 1
```

也就是说：**对"用自定义 profile 出包"的项目，rustopt 的 CI 门禁从定义上就量错了对象**（而且它自己的文档正推荐 `--profile dist` 这种双 profile 约定）。

修复：`plan`/`check` 新增 `--build-profile NAME`，把 profile 名一路传到 `variants::config_args`（`--config profile.<name>.<key>`）、argv、推荐块标题（`[profile.dist]`）与报告（`profile   dist`）。名字有白名单校验（字母/数字/`_`/`-`），因为它会被拼进 `--config` 键；**profile 不存在时 cargo 构建失败 → 退出码 2**，绝不会变成"通过"。

```
修复后
$ rustopt check --manifest . --budget 600KB --build-profile dist --no-log
  variant   current (what the package ships today)
  profile   dist
  command   cargo build --profile dist --message-format=json --locked
  measured  571.54 KB (571536 bytes)
  verdict   PASS  (within budget by 28.46 KB)      exit 0
```

对自己跑 `plan --build-profile dist --variants current,tuned` 还顺手验证了自洽性：本仓 `[profile.dist]` 已经是 `tuned` 组合，所以两个变体都是 571,536 B，推荐落在 `current`——这正是"测出来而不是猜出来"该有的样子。

---

## 9. 小项

| 项 | 事实 | 状态 |
|---|---|---|
| CLI 参数 | `Args::parse` 每次迭代 `argv[i].clone()` | **已修复**（借用 `&str`） |
| `guard::walk` | 每个 entry `is_dir()`+`is_file()` 两次 stat | **已修复**（`DirEntry::file_type()`；符号链接仍显式解析后跟随，因为"漏掉一个 ban"是这个模块最不能接受的失败） |
| 逐目录排序 + `scan` 再整体排序一次 | 重复排序 | 未改（量级 <1 ms，改动会动到扫描顺序语义） |
| ledger 写入 | `events::append` 每个事件 open/append/close；一次 plan = `2+2N` 次开文件 | 待数据（§12） |
| ledger 读取 | `ledger::read` 逐行 `serde_json::Value`；crate 里已有 `#[serde(tag="t")]` 的 `Event` 枚举可直用 | 待数据（§12）：改成 typed 会更严格，旧版/未来版事件缺字段时会被丢掉——容错是刻意设计，不能顺手改 |
| ledger 生命周期 | `runs.jsonl` 无上限、无 prune（本机 132 行 / 32 KB） | 待数据（§12） |
| ledger 位置 | `DEFAULT_LEDGER` 相对 **cwd** 而非 manifest 目录 | 不改：README 已把"只往工作目录写 ledger"当契约写明 |
| `--variants` | 只能选 8 个预置名，无 `--config key=value` 透传 | 待判断（§12） |
| 文档一致性 | README：「Not on crates.io yet」；[BASELINE-project-sizes.md](BASELINE-project-sizes.md):67：「它是已发布 crate」 | 未改（后者是本轮之外的调查文档） |
| CI | 已 pin 工具链 + rust-cache；测试里真实构建 fixture 是设计使然（本机 48+13 个测试，dev 档约 6 s 暖 / 20–30 s 冷） | **已加打包内容门禁**；自身体积门禁待数据（§12） |

---

## 10. 复现命令

```sh
# 图谱
CBM=/Users/echerlos/.local/bin/codebase-memory-mcp
$CBM cli index_repository '{"repo_path":"<repo>"}'
$CBM cli get_architecture '{"project":"Users-echerlos-syncfolder-project-dsfolder-rustopt","aspects":["all"]}'

# 插桩副本（不污染本仓；hotpath 0.28.4 已在本地 registry，可 --offline）
rsync -a --exclude target --exclude .git --exclude .rustopt ./ /tmp/rustopt-hp/
#   Cargo.toml: hotpath = { version = "0.28", optional = true } + [features] hotpath = ["dep:hotpath","hotpath/hotpath"]
#   函数上：#[cfg_attr(feature = "hotpath", hotpath::measure)]，main 上 #[hotpath::main]
cd /tmp/rustopt-hp && cargo build --release --features hotpath --bin rustopt
./target/release/rustopt plan --manifest <repo> --dry-run --no-log

# 落地后的验证
cargo test --locked && cargo clippy --all-targets --locked -- -D warnings && cargo fmt --check
cargo package --list --allow-dirty | grep -c '^docs/'          # 必须是 0
cargo build --locked --profile dist && rustopt check --manifest . --budget 600KB --build-profile dist --no-log
```

**测量纪律**：所有倍率都是同机、同输入、同二进制集下的 A/B，取 5–7 次中位数；`plan` 的墙钟对照两臂都用全新 work dir；字节对照逐项一致才认。单仓（12 个 unit、2 个依赖）的绝对数字不能外推到多依赖大仓，但"三遍扫描"与"两次 stat"这类比例与仓库大小无关。

---

## 11. 本轮落地清单

| 文件 | 改动 | 验证 |
|---|---|---|
| `src/guard.rs` | `scan_markers` 单遍扫描 + 原文预过滤；`strip_noncode` 改字节实现（并修 `\` 续行导致的证据行号 −1）；`walk` 用 `DirEntry::file_type` | 差异测试逐条一致；7.5× / 5.2× / 3.3×；`self_bootstrap_points_at_the_real_call_site` 等守护测试全绿 |
| `src/main.rs` | `dir_size` 单次 stat + 不跟随符号链接；`--build-profile` 解析与校验；`Args::parse` 去掉逐项 clone；usage 更新 | `dir_size_walks_recursively`、`profile_names_are_trimmed_and_validated`；`clean` 预览总量不变 |
| `src/plan.rs` | `package_meta ∥ toolchain ∥ guard::scan`（`thread::scope`）；`build_profile` 进入 `Plan`/`CheckReport` | 暖缓存 49.4 → 27.2 ms，5+5 次；13 个 CLI 测试 |
| `src/measure.rs` | `BuildOpts.profile` + `--profile <name>` argv；`collect_bin_artifacts` 类型化借用反序列化 + 预过滤 + 去掉整块拷贝 | 新增 4 个单测（含"诊断文本里出现 compiler-artifact 不算 artifact"）；6 MB 流 2.4× |
| `src/variants.rs` | `PROFILE_PREFIX` → `DEFAULT_PROFILE` + `profile_prefix()`；`config_args(v, profile)` | `config_args_follow_the_selected_profile` |
| `src/report.rs` | 推荐块标题按实际 profile 渲染；`check` 输出新增 `profile` 行 | CLI 测试断言 `profile   dist` 路径 |
| `Cargo.toml` | `exclude = ["docs/", "scripts/", ".github/"]` | `.crate` 126.1 → 40.4 KiB 压缩后 |
| `.github/workflows/ci.yml` | `Packaging content gate`：`cargo package --list` 不得列出 `docs/` | 本地按同样命令模拟通过 |
| `tests/` | tiny fixture 增加 `[profile.dist]`；新增 4 个 CLI 测试（命名 profile 门禁 / 不可构建 profile 退出 2 / profile 名注入 / `plan` 报告 profile） | 61 个测试全绿 |

---

## 12. 待后续迭代的数据（每条的触发条件与要收的数字）

1. **变体并行（§5，2.24×）**：先收三个数——`cores`、每变体构建时长、变体数。判据：当"变体数 × 单变体时长"里有 ≥40% 落在并行可重叠区（依赖编译阶段），且机器有 ≥4 空闲核时，才值得加 `--parallel-variants`。同时必须解决"价格失真"：并行后串行重测 recommended/default（暖 target dir 实测各约 35 ms）。
2. **cargo JSON 流式化（§6 内存口径）**：触发条件是真实大仓的 `plan` 父进程 RSS（本机小仓 9.2 MB 看不出来）。要收：`cargo build --message-format=json` 的 stdout 字节数、`plan` 的 peak RSS。当 stdout > 32 MB 或 RSS 明显高于子进程本身时，再改成 piped 逐行读 + stderr 独立线程增量哈希。
3. **自身体积门禁**：`check --build-profile dist` 现在能工作了，但预算需要三平台各一个数字（CI 只在 ubuntu 跑，release 资产有 macos-arm64 / linux-x86_64-musl / windows-x86_64 三个）。要收：三个目标各自的 dist 字节数，取"实测 + 固定余量"作为 `--budget`，再放进 release job（tag 前）。本轮不猜——那正是这个工具反对的事。
4. **ledger 三项（§9）**：触发条件是 `runs.jsonl` 行数或读耗时。要收：单文件行数、`ledger --tail 20` 耗时。到"读一次 > 100 ms"或"文件 > 10 MB"再动（typed `Event` 反序列化 + 单 writer + `--prune`）。
5. **`--config` 透传（§9）**：不是性能问题，是"用户已有 profile + 想加一个 knob"的场景。判据：出现第一次"预置 8 个变体都不匹配用户问题"的真实反馈。
6. **体积/速度的账（§0 末段）**：本轮修复让 dist +6.3%（537,712 → 571,536 B）。判据：若发布物体积成为对外承诺（例如 README 里出现具体数字），就用 `check --build-profile dist` 在 CI 钉住，届时再决定是否为了几十 KB 回退某些改动。
