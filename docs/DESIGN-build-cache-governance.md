# 设计文档：构建速度与产物体积治理

状态：草案 v1（2026-10-07）· 适用：`~/syncfolder/project` 下的 Rust 仓群（当前 24 个 `Cargo.lock`、14 个 `target/`）
依据：[FINDINGS.md](FINDINGS.md)（判定表与实测基线）· 配套：[bottleneck-verified.md](bottleneck-verified.md)、[optimization-space.md](optimization-space.md)

---

## 1. 目标与非目标

**目标**

1. 让"构建时间"与"磁盘占用"变成**可度量、可回归、可回滚**的工程项，而不是感觉。
2. 在 **stable 工具链 + 不改产品依赖** 的约束下，把能稳定重复的那部分收益**全部拿到**（实测约 10–20% 墙钟、40% dev 磁盘、跨项目复用）。
3. 把"不稳定/产品级"的选项（nightly 并行前端、改依赖图）**显式排除在默认策略之外**，只作为按项目、有验收的可选项。
4. 清理从"一次性动作"变成"**清一次 + 防再生**"的闭环。

**非目标**

* 追求单一魔法开关（实测不存在：见 FINDINGS §0）。
* 把 nightly 引入 CI 或 release。
* 自研缓存服务器（sccache 已有本地/远端/多级能力）。
* 重写依赖图（属各项目的产品决策，本设计只提供判据与门禁）。

---

## 2. 设计原则（每条都有实测来源）

| # | 原则 | 来源 |
|---|---|---|
| P1 | **先算下界再动手**：`T ≥ max(L, W/m)`；路径受限（`L` 主导）就不要投并行/加核 | FINDINGS §4.1：verify-gate 8.0 s vs 44.5/10=4.45 s |
| P2 | **稳定性优先**：默认策略只用 stable、不改指纹、可回滚（稳定性五条判据见 FINDINGS §1） | — |
| P3 | **四个口径分开报**：墙钟 / CPU / 磁盘(du) / 缓存命中率，禁止混用 | `debug=0` 墙钟仅 −12% 而磁盘 −40%；sccache CPU −76% 而墙钟可正可负 |
| P4 | **复用的前提是 key 稳定**：版本、绝对路径、feature、环境四类自由度越小越好 | 显式 `CARGO_TARGET_DIR` ⇒ 命中 21→17；版本分叉 ⇒ 5/17 |
| P5 | **清理必须配防再生**，否则是仪式 | 6 个仓仍在长 incremental（5.16 GB） |
| P6 | **两段式 + fail-closed + 台账**（沿用既有工具纪律） | 既有 `rust-target-audit.py` 的设计 |

---

## 3. 策略（Policy）

### 3.1 编译档案（每仓 `Cargo.toml`）

```toml
[profile.dev]
incremental = false          # 换：不再长 incremental；代价是单 crate 重编 ~0.29→0.69 s（实测）

[profile.dev.package."*"]
debug = 0                    # 依赖不带调试信息；自身 crate 仍可断点（实测：与全关同为 225 M）

[profile.release]            # 保持 cargo 默认（构建快）
[profile.dist]               # 出包专用：opt-level="z" + lto="fat" + cgu=1 + strip="symbols"
inherits = "release"
```
* **不使用 nightly 作为默认**；若某项目要 `-Zthreads`，用**日期版** nightly 且仅限本机 dev，并在 CI 保持 stable（并在该仓 README 写明）。
* 每仓加 `rust-toolchain.toml` 钉到 CI 同版本（现状：只有 unirun 有；rustopt 本地默认 `stable` 而 CI 钉 1.97.0，属于漂移源）。

### 3.2 缓存（全局 `~/.cargo/config.toml`，已落地）

```toml
[build]
rustc-wrapper = "sccache"
[env]
SCCACHE_IGNORE_SERVER_IO_ERROR = "1"
```
**硬性约束**：

* **禁止**为了"隔离"而在任何地方设置 `CARGO_TARGET_DIR`（会让跨项目复用静默归零）。唯一例外：**rust-analyzer 专用 target dir**（它不参与复用，但能避免 RA 的 check 产物驱逐你的 build 产物）。
* 需要"多重目标目录"的场景（如 rustopt 的 per-variant 隔离）必须**先实测**"同 target dir 放多套配置"是否更优，再决定。
* CI 侧补齐：`sccache-action`（GH Actions 路径跨运行固定 ⇒ 命中率最高）；容器侧用 BuildKit `--mount=type=cache`。

### 3.3 版本（lockfile）

* **门禁指标**：**可对齐版本分支数 = 0**（同兼容族群内多版本；dsfolder 已 14→0）。检查脚本见 §5。
* **跨族群重复**（本机 13 条：`sha2` 0.10/0.11、`syn` 2/3、`hashbrown`、`windows-sys`、`rand`、`getrandom`、`webpki-roots` …）**不强制合并**，只登记 + 按需随上游升级；判断"是否参与构建"必须用 `cargo tree -e normal`（实测：`syn 2.x` 在 unirun/session-index 仅 dev/bench）。
* 合并 workspace 只在 **profile 政策一致**的子集做（成员 `[profile.*]` 会被忽略并告警；`session-index` 的 release 是 `opt-level="z"`，与另 6 个冲突）。

### 3.4 清理

* **口径**：du（按 inode 去重）优先，打印虚高倍数；`.o` 永不进清单（硬链接共享 inode：unirun 13,866 路径 / 6,856 inode）。
* **分类**：`incremental` / `superseded`（同 `(crate,ext)` 只留最新 + **年龄门 7 天**，只认 `.rlib/.rmeta/.dylib/.a`）/ `cross-triple`（宿主 triple 取自 `rustc -vV`，取不到就一个不碰）/ `flycheck`。
* **闸门**：`target/<profile>/.cargo-lock` 的 `flock` 非阻塞探测；`ps` 只作兜底；`ps` 不可用也拒绝（fail-closed）。
* **执行**：两段式（预览 → `--apply`）；**直接删除**（硬链接进废纸篓不释放 inode）；每次追加 JSONL 台账；删前复核 `dev/ino/mtime_ns`。
* **防再生**：清理后立刻核对 §3.1 的 `incremental = false`（按**配置文件**判断，不按"目录是否存在"判断）。
* **节奏**：每批交付收尾清 `build-stale`；每月全量看年龄分布。
* **机器级**：`~/.rustup` 无引用工具链（5.85 GB，走人工确认清单）、`~/.cargo/registry/src`（1.9 GB，可重新解包）、sccache 缓存目录（上限默认 10 GiB）。

---

## 4. 度量与门禁（SLO）

| 指标 | 采集 | 基线（verify-gate） | 目标 / 门禁 |
|---|---|---|---|
| 冷编墙钟 | `--timings` 或 hyperfine（全新 target dir，串行） | 6.97 s | 相对回归 >10% 报警 |
| 内循环墙钟（改一行） | `touch` + `time cargo build` | 0.87 s | >1.2 s 报警 |
| dev target du | `du -sh target` | 233 M | >250 M 报警 |
| 缓存命中率（重复构建） | `sccache --zero-stats` → build → `--show-stats` | 106 命中 / 0 未命中（keys 匹配） | <50% 报警（说明 key 漂移） |
| CPU 时间（同一构建） | `/usr/bin/time -p` | 6.97 s 全冷 / 3.29 s 热 | 记录趋势 |
| **可对齐版本分支数** | 脚本（§5） | 0（dsfolder） | **≠0 直接 fail** |
| 关键路径与尾巴 | `--timings` 的 unit 数据 | `L≈7.0–8.3 s`，尾巴 25% | 记录趋势；`L` 增长 >20% 报警 |
| incremental 目录 | `du` + 配置核对 | 0（已关的仓） | 出现即报警 |

**"墙钟 vs CPU 必须同时看"**是本设计的一条硬规则：sccache 让 CPU −76% 而墙钟可能不变（tail-bound）；`-Zthreads` 让墙钟 −12.8% 而 CPU +17%。只看一个数会得出相反结论。

---

## 5. 实现设计（落点：`rustopt`）

`rustopt` 已有：`plan`（尺寸变体矩阵 + 构建代价）、`check`（预算门禁）、`ledger`（JSONL 台账）、`clean`（work dir）。新增/改造四个子命令，全部沿用现有契约（`0` 通过 / `1` 违反门禁 / `2` 工具错误；`--emit json|pretty`；`--log` 台账）。

### 5.1 `rustopt doctor`（新增，最高性价比）

只读体检，输出**可执行修复清单**。检查项与判据：

| 检查 | 判据 | 严重度 |
|---|---|---|
| profile 政策 | `[profile.dev] incremental = false` 且 `[profile.dev.package."*"] debug = 0` | warn |
| 工具链一致性 | 有 `rust-toolchain.toml` 且与 CI 钉的版本一致；无则 warn（漂移源） | warn |
| **target dir 污染** | 环境变量或 `.cargo/config.toml` 里出现 `CARGO_TARGET_DIR`（除 RA 专用） | **error**（静默毁掉复用） |
| 版本分叉 | 计算"可对齐分支数"；>0 报错并给出 `cargo update -p X --precise V` 清单 | **error** |
| 缓存接线 | `build.rustc-wrapper` 存在；`sccache --show-stats` 可用 | warn |
| 磁盘预算 | `du target` 超阈值；`incremental` 目录非空但配置已关（残留） | warn |
| 新鲜度风险 | 项目路径落在云同步目录内（mtime 判据会被外部工具扰动） | info |

验收：`--emit json` 字段稳定；对已知反例（本机 dsfolder 7 仓）输出应为"0 error"；对故意设置 `CARGO_TARGET_DIR` 的临时仓必须报 error。

### 5.2 `rustopt cache`（新增）

1. **no-op 探针**：连跑两次 `cargo build`，报第二次墙钟（基线 0.07 s / 112 unit）；
2. **指纹归因**：`CARGO_LOG=cargo::core::compiler::fingerprint=info` 的 `dirty:` 分类（EnvVarChanged / RUSTFLAGS / features / mtime）；
3. **sccache 探针**：`--zero-stats` → 两次构建 → 解析命中率，并**同时报告墙钟与 CPU 变化**（区分 §4 的两个口径）；
4. **key 稳定性审计**：环境里是否有 `RUSTFLAGS`/`CARGO_TARGET_DIR`/`CARGO_PROFILE_*` 与配置文件冲突。

### 5.3 `rustopt target`（把既有纪律产品化）

不要重写 `scripts/rust-target-audit.py`：把它的判据接进来（du 优先 + 虚高倍数 + 年龄门 + flock 闸门 + 两段式 + 台账），并把 `rustopt clean` 现在"整根 work 目录删"的行为改成**按目录挑 + 年龄门**。
**先读的现成实现**（2026-10-06 已核实）：`cargo-orphan-gc`（crates.io 真实存在但仅 38 次下载、会改指纹、必须先用 shadow 模式；其**不变式表**值得直接吸收：无成功替换不回收 / 未知所有权泄漏 / 活跃输入加租约 / fail-closed / dry-run 默认为真）；Dune 的 `dune cache trim`（把 **link count > 1** 判为不可回收开销 —— 与本机 `.o` 的结论同源）。

### 5.4 `rustopt plan --time-budget`（已有数据的增量）

`measure::Outcome.duration_ms` 已在手：输出**尺寸 × 时间**的 Pareto 前沿，并支持"不超过默认 profile N 倍时间"的约束。配套：报告"关键路径份额"而不只是总数（本次实测证明单看总耗时会漏掉 25% 的链条）。

---

## 6. 分阶段落地

| 阶段 | 内容 | 验收 | 回滚 |
|---|---|---|---|
| **0（已完成）** | dsfolder 7 仓版本对齐；verify-gate profile 试点（`package."*".debug=0`）；全局 sccache 接线 | 7/7 build+test ✓、MSRV ✓；target 999 M→233 M；真仓跨项目 9 命中 | 见 FINDINGS §5 |
| **1（低风险，待你确认）** | ① 6 个再生 incremental 的仓加 `incremental=false`；② 把 profile 政策推广到另 6 个 dsfolder 仓；③ 每仓加 `rust-toolchain.toml` | 每仓 build+test ✓；`du` 不再增长；无新报警 | 逐仓 `git checkout -- Cargo.toml` |
| **2（工具化）** | 实现 `rustopt doctor` + `cache`；把"可对齐分支数 = 0"与"target du 预算"接进 CI（fail 而非 warn） | `doctor` 在 7 仓输出 0 error；CI 上故意引入分叉会被拦住 | 关掉对应 job |
| **3（按需）** | ① CI 接 `sccache-action`；② 结构级去依赖（verify-gate 的 ICU 链 / `sha2` 族群）；③ 跨机二级缓存（NAS 上 S3 或 WebDAV，M1↔M3）；④ rustopt 自身变体 target dir 的重新设计（先实测） | 各自的"改前测 → 改 → 改后测"三件套 | 逐项 |

---

## 7. 风险与未决

| 风险 | 说明 | 缓解 |
|---|---|---|
| **缓存命中率静默归零** | 任何人设了 `CARGO_TARGET_DIR`、动了版本/feature、换了工具链，命中率就掉，而构建仍然成功 | `rustopt cache` 把命中率做成常规指标；`doctor` 把 `CARGO_TARGET_DIR` 判为 error |
| 远端二级缓存的信任边界 | 跨机/跨仓库共享产物需要明确谁能写 | 阶段 3 先只读二级、限定局域网；不引入第三方云端点 |
| profile 切换的磁盘尖峰 | 改 profile 后新旧产物并存（实测 782 M→990 M） | 变更清单里必须含"清一次"（或细档清理） |
| nightly 可选项侵蚀默认 | 一旦某仓默认 nightly，CI/release 与本地分裂 | 策略层禁止；只允许日期版 + 本机 dev + README 注明 |
| 判定表过期 | 上游变化会翻转结论（并行前端稳定化、cross-workspace cache、build-dir 稳定） | FINDINGS §7 列出触发条件，触发即重跑 |

**未决问题**：rustopt 的 per-variant target dir 是否该合并（需实测）；NAS 二级缓存的形态（S3 vs WebDAV）；`~/.rustup` 5.85 GB 的例外清单由你确认；verify-gate 的 `syn 2.x` 是否值得为它升级 `ureq/url` 栈。

---

## 8. 复现与验收命令

```sh
# 下界判定：路径受限还是吞吐受限
cd <repo> && rm -rf /tmp/t && CARGO_TARGET_DIR=/tmp/t cargo build --locked --timings
python3 - <<'PY'   # 从 target/cargo-timings/cargo-timing.html 读 UNIT_DATA，算 W、L、并发曲线
PY

# 缓存命中率（必须同时看墙钟与 CPU）
sccache --zero-stats
rm -rf target && /usr/bin/time -p cargo build --locked        # 热：期望 hits>>misses
mv ~/.cargo/config.toml /tmp/parked && rm -rf target && /usr/bin/time -p cargo build --locked
mv /tmp/parked ~/.cargo/config.toml                            # 冷：作为对照

# 版本分叉门禁
python3 - <<'PY'   # 解析各仓 Cargo.lock，按兼容族群分组，输出"可对齐分支数"（目标 0）
PY

# 清理（两段式）
python3 ~/syncfolder/project/dsfolder/scripts/rust-target-audit.py status
python3 ~/syncfolder/project/dsfolder/scripts/rust-target-audit.py clean --class incremental --apply
```
