# shaders 模式迁移：测量口径、variant 并行与台账有界化

**状态**：方案（未开工）。本文只登记落点与验收判据。

- 日期：2026-10-07
- 来源：跨项目调研报告（los 工作区 `docs/research/2026-10-07-shaders-patterns-cross-project-optimization.md`，含逐条 `file:line` 与三档证据分级）
- 方法/纪律：技能 `transferable-pattern-audit`；规则 `~/.claude/rules/pattern-transfer-discipline.md`
- 相关既有文档：[SELF-OPTIMIZATION-REVIEW.md](./SELF-OPTIMIZATION-REVIEW.md)、[optimization-space.md](./optimization-space.md)、[bottleneck-verified.md](./bottleneck-verified.md)、设计文档 `../RUSTOPT-SIZE-HARNESS-DESIGN-2026-10-06.md`

---

## 0. 前提：本仓的自我优化欠债是**有数据支撑的决定**，不是缺陷

[SELF-OPTIMIZATION-REVIEW.md](./SELF-OPTIMIZATION-REVIEW.md) §12 记录自我优化让 dist 档 537,712 → **571,536 B（+6.3%）**、release 档 940,720 → **1,024,816 B（+8.9%）**，并把它作为按数据决定的欠债接受。**本文不把它列为待修项**，只按下面的 `D-4` 把它纳入等价强度分级，避免它被当成「回归」反复重开。

本仓的自审质量在七个被调研项目中最高（guard 7.5×、measure 2.4×、preflight 1.82×，每条带实测 delta）。所以下面的项集中在**测量口径、并行调度、台账 IO** 三处，且都已有实测头寸或明确 open 标记。

---

## 1. 决策

### D-1 `current` variant 必须读 manifest 实际生效的 `[profile.release]`

**问题（CONFIRMED，跨仓交叉印证）**：
- 本仓已修过一次同类缺陷：`check` 在产物由其它 profile 产生时仍测 `--release`，报 940,720 B 而实际交付 571,536 B，为此加了 `--build-profile`；
- 下游 `cantool/scripts/size-gate.sh:19-27` 与 `cantool/TODO.md:24` 记录：本工具 advisory 报 **30,867,280 B**，实际交付 **29,424,720 B**，**差 1.44 MB，cause open**。症状指向 **`current` variant 测的是 cargo 内建默认值，而不是 manifest 里的 `[profile.release]`**。cantool 目前用「强制交叉校验真实产物」绕过。

**形态（迁移）**：`shaders` 的 `pipelineCache` 用 `activeHash` 明确标识「当前正在被渲染的那一个」，并且**只有新的那个成功画出第一帧之后**才把 active 换成它（`markReady`）。等价纪律：**任何测量/门禁都必须绑定到「将要交付的那个产物的身份」，而不是一个同名但来源不同的产物。**

**做法**：
1. `current` variant 从 manifest **解析**实际生效的 `[profile.release]`，而不是用 cargo 内建默认；
2. 每次测量记录 `(artifact sha256, length, effective profile, manifest hash)` 四元组，进 `runs.jsonl`；
3. `check` 断言「被测产物 == 交付产物」；
4. **回归 fixture**：一个 `[profile.release]` 非默认的 fixture manifest，必须让 `current` 与 `default` 产生**不同**结果 —— 当前若 `current ≡ default` 即 bug 仍在。

**验收**：fixture 下 `current != default`；四元组进台账；cantool 侧 1.44 MB 差消失且其交叉校验从「绕过」降级为「断言」。

**这条同时恢复本仓的核心判据**：设计文档 §0 的「它必须能在没参与开发的仓上给出人想不到的结论」。

### D-2 variant 矩阵并行 + 显式并发上限

**问题（CONFIRMED）**：[SELF-OPTIMIZATION-REVIEW.md](./SELF-OPTIMIZATION-REVIEW.md) §0 记录 variant 矩阵仍串行，**已实测 2.24× 头寸**（5 个 variant 冷启动 34.35 s → 15.36 s），但**被推迟**（理由："gated on adding a `-j` cap"）。

**形态（迁移）**：variant 之间是**完全独立的工作项**（各自独立 `CARGO_TARGET_DIR`，位于 `~/.cache/rustopt/work/<fnv(repo)>/<variant>`）。`shaders` 对应的是 `dispatcher.dispatch(steps)` 执行一个**有序的独立步骤列表** —— 它保持总序但允许批处理，并且**明确把批处理编码器记为「可分离的后续优化」**。另配「用显式预算封住搜索」的思路（其 `PUSHDOWN_MAX_NODES = 24`）。

**做法**：
1. 加 `-j <n>`，默认取 `min(variants, cores/2)` 或按可用内存推导（**保守默认 + 可调**，无界并行会把 cargo 的 IO/内存压垮）；
2. 保持 `--locked` 与每 variant 独立 target dir 的隔离（并行前提，已具备）；
3. 断言「串行 vs 并行给出相同的 variant 集合与 verdict，只有时间不同」；
4. 并发度写进 `runs.jsonl` 作为记录字段。

**验收**：5 variant 冷启动 ≤ 16 s（当前 34.35 s）；串并行结果一致性断言全绿。

### D-3 ledger/events：单次合并写 + 有界保留

**问题（CONFIRMED）**：[SELF-OPTIMIZATION-REVIEW.md](./SELF-OPTIMIZATION-REVIEW.md) §0 记录 —— 每个 plan 有 **`2+2N` 次文件 open/close**；`runs.jsonl` **无界且无 prune**；读取时用 `Value` 重新解析。明确标注为 open item。

**形态（迁移）**：`shaders` 两件事一次解决 ——
1. **dirty 合并**：`uniformStore.flush()` 把一帧内所有写入合并成**一次** patch（注释原文：「Patches accumulate across a frame and flush once」）；
2. **有界 + dispose**：`pipelineCache` 的 LRU 有显式容量、**永不驱逐活跃项**、驱逐时调用 `dispose`。

**做法**：
1. ledger 在整个 plan 期间**只 open 一次**；事件写内存缓冲，plan 结束时一次追加写。若崩溃可见性是硬需求，则保留「每事件一次 append」但改为**单个持久 fd + 缓冲写**，避免 open/close 系统调用；
2. `runs.jsonl` 加保留策略（按时间 + 条数双上限，或按项目滚动），prune 时保留「最近 N 条 + 每月一条摘要」；
3. 读取改流式反序列化（**参照 `measure.rs` 已完成的 `Value` → typed 改造，那里拿到 2.4×**）。

**必须保持的性质**：append-only 且只存 `stderr_hash` + 长度的既有设计是对的，**不要改语义，只改 IO 形态与保留策略**。

**验收**：plan 的 open/close 次数从 `2+2N` 降到常数（可计数）；`runs.jsonl` 有上界；读取不再全量 `Value` 解析。

### D-4 变更等价强度分级（Gate A/B/C）+ bail-out + 异类清单

**形态（迁移）**：`shaders` 的 `kit/PRIMITIVES.md:100-146`：
- **A 字节等价**：产物字节必须相同，**不许移动既有基线**，无需人眼；
- **B 论证等价**：文本变了但可证相同，需更新基线 + reviewer 读 diff；
- **C 会改变行为/尺寸**：需签核 + 记入变更日志。

**两个「看起来像 A 但不是」的陷阱**：
1. **浮点重排**（本仓对 probe 数值敏感）；
2. **重命名** —— **本仓的守卫匹配的是源码文本**，改名会移动守卫结果。这条与 shaders「函数名会进生成的 WGSL」是同一陷阱。

**配套**：**bail-out 规则**（达不到字节等价就跳过该处并记录原因，不得为异类扭曲自己）+ **显式异类清单**（永久异类合法）。本仓 variant 层面的「permanent outliers」正好需要这个：某些 crate 对小尺寸优化不敏感，应被**显式记为异类**，而不是被工具反复尝试。

**用途**：把 §0 的 +6.3%/+8.9% 欠债正式标为 Gate C 记录，阻止后续会话重复开审。

### D-5 守卫扫描：注释剥离的夹具化

**问题（CONFIRMED）**：本仓已修过一次「off-by-one 让证据指向一个注释」（自审原文：「一个指着注释的 ban 比没有 ban 更糟」），现做法是 "guards matched on code-only with path-segment needles"。

**形态（迁移）**：`shaders` 的 `enumPropSweep.test.ts` **先剥注释再匹配**，并在注释里写明理由（防止参考注释造成假阴性）。

**做法**：在 `tests/fixtures/` 补两个夹具 ——（a）**在注释里**包含被禁 needle 的 crate，断言守卫**不**命中；（b）**在代码里**包含该 needle 的，断言守卫**命中**。把「已经踩过的坑」变成不可回归的资产。

### D-6 设计文档的拒绝清单编号化

**问题（CONFIRMED）**：设计文档 §6 已有一份**拒绝清单**，每条带一行理由（无 `target/` GC、无 what-if 估算器、无单态化去重、无二进制打包、无动态链接建议）。这是好实践，但它是散文段落而不是可检索的编号项。

**形态（迁移）**：`shaders` 的 `PRIMITIVES.md` 把约定做成 **`C1–C9` 编号规则 + `D-1`/`D-2`/`D-6` 已解决约定 + 「实现中学到的」章节 + 每类别的显式异类清单**，并**显式区分维护者文档与使用者文档**。

特别值得抄的一条：其 `D-2`（两个亮度标准都保留，迁移时**采用该文件今天使用的权重**以保持 Gate A）是极好的范例 —— **拒绝统一化有时比统一化更正确**。

**做法**：把拒绝清单改成 `D-n` 编号项（与本文件的 `D-n` 不冲突：本文件的 D-n 是本迁移方案的决策，设计文档的 D-n 是工具自身的既有约定 —— 落地时需统一命名空间以避免混淆）。

### D-7 幂等发布

本仓已有：`docs/` 绝不进 `.crate` 的**打包内容门禁**、`msrv` job 交叉校验 `rust-version`、tag 触发的 3 目标 release 矩阵、`.crate` 从 126.1 → 41.2 KiB。

**唯一补充（迁移）**：`shaders` 的**幂等发布** —— release workflow 在 `npm view <pkg>@<version>` 能解析时**跳过**发布，避免「发布后失败再跑」时的 409。本仓的 3 目标矩阵若会在部分目标成功、部分失败后重跑，需要同等幂等性。

---

## 2. 不采纳清单

| 项 | 理由 |
|---|---|
| `swap-when-ready`（先证明再提升）用于 variant 矩阵 | 批处理无「服务连续性」消费者；这里正确的形态是**并行调度**（`D-2`）而不是双缓冲提升门 |
| `shaders` 的 LRU 容量等场景常数 | 编辑场景经验值，必须按本仓真实工作集推导（`D-3` 的保留策略阈值） |
| 无界 variant 并行 | 会把 cargo 的内存/IO 压垮；`D-2` 要求显式上限 |
| what-if 估算器 / 单态化去重 / 二进制打包 / 动态链接建议 | 设计文档 §6 已逐条给出拒绝理由；本方案不重开 |
| 把 §0 的 +6.3%/+8.9% 当回归处理 | 它是有数据支撑的 Gate C 决定；按 `D-4` 记录而不是反复开审 |

---

## 3. 优先级与顺序

| 序 | 决策 | 依赖 | 规模 |
|---|---|---|---|
| 1 | `D-1` `current` 读 manifest + fixture | 无（**解除下游 cantool 的绕过**） | 小—中 |
| 2 | `D-3` ledger 单次写 + 有界 | 无 | 小 |
| 3 | `D-2` variant 并行 + `-j` | 无（已实测头寸） | 小—中 |
| 4 | `D-5` 守卫夹具 | 无 | 小 |
| 5 | `D-4` Gate 分级 + 异类清单 | `D-1` 的四元组是基础 | 小（流程） |
| 6 | `D-6` 拒绝清单编号化 | 无 | 小 |
| 7 | `D-7` 幂等发布 | 无 | 小 |

## 4. 风险

| 风险 | 缓解 |
|---|---|
| `D-1` 读 manifest 后，`current` 与 `default` 仍然相同（说明解析没生效） | fixture 是**必要条件**：fixture 下两者必须不同 |
| `D-2` 并行导致单 variant 变慢或 OOM | `-j` 默认保守 + 可调；并发度进台账，可回归 |
| `D-3` 合并写牺牲崩溃可见性 | 先量清崩溃可见性是否真的是需求；若需要则保留每事件 append 但换单 fd |
| `D-5` 夹具与真实守卫行为漂移 | 夹具与守卫共用同一匹配函数（不复制逻辑） |

## 5. 未决问题

1. `D-1` 的 fixture 放 `tests/fixtures/` 下时，是否会与既有 `manifest.toml` 命名约束（`cargo package` 排除含 `Cargo.toml` 的子目录）冲突？
2. `D-3` 的保留策略阈值取多少？（需要先量出 `runs.jsonl` 的实际增长速度）
3. `D-6` 的编号命名空间：本迁移方案的 `D-n` 与设计文档既有的 `D-n` 如何区分？
