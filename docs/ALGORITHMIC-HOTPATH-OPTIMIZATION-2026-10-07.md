# 变体搜索与测量决策：算法热点优化设计

**状态**：设计（未实现）
**日期**：2026-10-07
**来源**：`dsfolder/MATH-TO-CODE-OPTIMIZATION-RESEARCH-2026-10-07.md`；原始调研证据 `dsfolder/HOTPATH-SURVEY-RUSTOPT-AND-COMFYUI-2026-10-07.md`
**方法**：skill `algorithmic-hotpath-audit`；决策纪律 `~/.claude/rules/algorithmic-hotpath-discipline.md`
**调研范围**：只读。`CONFIRMED` = 读过代码并给出 file:line；`INFERRED` = 已标注。

---

## 0. 结论

rustopt 的问题**不是搜索空间太小，而是决策没有统计依据**。这在本项目里格外讽刺：工具自己文档化了测得的漂移，却用裸 argmin 做判断。

| 优先级 | 位置 | 问题 | 目标 |
|---|---|---|---|
| P0 | `src/plan.rs:253-263` | 裸 argmin，零余量；而 `tests/cli.rs:157-169` 文档化 0.5% 容忍漂移 | 最小效应量 / tie band |
| P0 | `src/measure.rs:150-158` | 构建时间**单次采样**，且循环位置与变体混淆 | 交错重复采样 + 中位数/区间 |
| P1 | `src/plan.rs:175` vs `:432`；`variants.rs:53-66` | 无 `[profile.release]` 时 `current` 与 `default` 是同一构建 | manifest 感知的等价检查（省 1/5–1/8 构建） |
| P1 | `src/variants.rs:52-120` + `plan.rs:174-201` | 8 点手工目录（默认 5 点）覆盖 ~7.7e3 格；构建串行 | 结构化搜索 + 有界并发 |
| P2 | `src/main.rs:243` | 工作目录标签 FNV-1a64 **截断到 32 位** | 加宽（~77k 路径时当前 50% 碰撞） |

**一条关键洞察**：**size 是确定性的**（`measure.rs:207-217` 直接 `metadata().len()`），所以 **size 轴没有臂噪声**。这是本项目的幸运之处——噪声问题只存在于 build time 轴。必须利用这一点：不要为 size 花采样预算。

---

## 1. P0：决策的统计纪律

### 1.1 现状（CONFIRMED）

`src/plan.rs:253-263`：

```rust
if best.is_none_or(|(_, bb)| b < bb) { best = Some((v, b)); }
```

**裸 argmin，零余量。**

而同类事实在仓库里已被文档化并被测试强制：

- `tests/cli.rs:157-169`：语义相同的配置**并不逐字节相同**——"forcing the options changes the option hash that gets embedded in the artifact, which is a few dozen bytes"，容忍 `drift < 0.005`（**0.5%**）。
- `README.md:19-27`：真实输出里胜出的 argmin 旁边就是 `current` vs `default` 的 **64 字节（0.015%）** 差距。
- `src/measure.rs:150-158`：构建时间**单次采样**，且**循环位置与变体混淆**（`current` 最先跑，冷页缓存；`tuned` 最后跑，依赖已热）。`price_ratio_vs_default`（`plan.rs:288-291`）继承同一问题。
- 整个 `src/` 与 `tests/` 里没有 `repeat/median/stddev/confidence/bootstrap`。

### 1.2 改造

1. **引入最小效应量 / tie band**：复用已存在的 `budget::pct_delta`（`budget.rs:66-71`）；建议 band ≥ 工具自己文档化的 0.5% 漂移。
2. **size 轴不重复采样**（确定性，重复无信息量）。
3. **build time 轴用交错重复臂**（A/B/A/B × k，取中位数 + 置信区间）。
4. 当候选差异落在 band 内时，**输出并列而不是武断选一个**——这是唯一诚实的结论。

### 1.3 收益与风险

- **收益**：推荐不再在无意义的字节差上翻转；`vs_current_*` 的符号变得可信。
- **风险**：低。这是**最小改动、最高确定性收益**的一项，建议第一个做。

### 1.4 验收

- 用当前 README 的 fixture（`current` 与 `default` 差 64 字节）做测试用例：**必须输出"在容差内并列"而不是给出推荐**。
- 构建时间的置信区间宽度必须报告出来，供使用者判断采样是否足够。

---

## 2. P1：搜索设计——8 点手工目录 vs ~7.7e3 点格

### 2.1 现状（CONFIRMED + INFERRED）

`src/variants.rs:52-120`：`pub const VARIANTS: &[Variant] = &[...]` 是**手写的 8 项目录**（`current, default, z, strip, lto, thin, tuned, abort`），默认集只有 5 个（`default_set: true` 在 `:57, :64, :71, :85, :104`）。只有 `tuned`（4 旋钮）与 `abort`（= tuned + `panic="abort"`）是多旋钮点。**从不变化** `incremental, debug, overflow-checks, debug-assertions, rpath, split-debuginfo`。

**INFERRED**：cargo release profile 的合理取值笛卡尔积约 **7.7e3 点**（5 opt-level × 4 lto × 3 cgu × 4 strip × 2 panic × 2 debug × 2 overflow-checks × 2 debug-assertions × 2 incremental）⇒ 8 个实测点约覆盖 **0.1%**。

**工具自己的 README `:44-47` 记录了非单调交互**：`opt-level="z"` 单独用比 cargo 默认**更大**（真实 crate 上观察到 **+9% / +14%**）。**这正是固定手挑设计无法声称最优的区间，也是坐标下降会卡在局部最优的证据。**

`src/plan.rs:174-201` 构建**严格串行**：

```rust
for v in &opts.variants {
    let vdir = opts.work_dir.join(v.name);
    ...
    let outcome = measure::build(&canonical, v, &vdir, &opts.build)?;
```

只有 `cargo metadata` / `rustc -vV` / `guard::scan` 三者并行（`plan.rs:130-135`）。

### 2.2 改造

1. **先明确目标**：从"a smaller variant"改为"**在声明的置信度下最小的 variant**"。当前的模糊目标使正确性问题无法被讨论。
2. **主导剪枝（dominance pruning）**：size 确定性 ⇒ 可在旋钮偏序上做 best-first 搜索，用已测 incumbent 剪枝。这就是"生成多、存储少"：枚举候选，只保留 Pareto 前沿。
3. **非单调交互的存在意味着不要用简单坐标下降**；用 best-first / beam + 界。
4. **不要期待穷举**。降到"每个旋钮的关键转折点 + 交互项"，本质上是**部分因子设计 / covering array** 的思路——**诚实标注：这部分属于实验设计，不在 math 合集范围内，也不在本项目既有文档中。**
5. **有界并发**（2–3，各自仍用 cargo `-j`）可缩短墙钟且**尺寸逐字节相同**（size 确定性）；但**必须把 `duration_ms` 移到单独的串行/交错测量通道**，否则时间估计被并发污染。

### 2.3 收益与风险

- **收益**：从"一个更小的变体"到"可辩护的最小变体"；覆盖率从 0.1% 提升到有结构的搜索。
- **风险**：中。搜索空间需要时间预算上限；必须保持"**不编辑用户文件**"的既有约定（README 明确写着 copy the block yourself）。

### 2.4 验收

- 结构化搜索在 fixture 上必须找到 `tuned`（已知最优），且**不能比穷举差**（fixture 上可穷举验证）。
- 搜索必须报告"用了几次构建 / 访问了多少个格点 / 剪掉了多少"。
- 并发模式下 size 结果必须与串行模式**逐字节相同**。

---

## 3. P1：冗余构建与回退循环

### 3.1 现状（CONFIRMED）

| 位置 | 现状 |
|---|---|
| `plan.rs:175` vs `:432`；`variants.rs:53-66` | `current` 不传 `--config`，`default` 显式写出 cargo 默认 ⇒ **manifest 无 `[profile.release]` 时二者是同一个构建**（那 64 字节差距就是冗余证明）。**`guard.rs:454-469` 已经解析了 manifest** ⇒ 一个"无 profile 覆盖"检查即可省掉 5–8 次构建中的 1 次 |
| `src/main.rs:243` | 工作目录标签是 FNV-1a64 **截断到 32 位**（`[..8]`）⇒ 约 77k 条路径时 50% 碰撞概率 ⇒ **静默共享 target 目录** |
| `guard.rs:255-262` | `scan_markers` 按每行 × 每组 × 每 needle 匹配；单趟 Aho–Corasick 是经典替代。**注意**：`docs/SELF-OPTIMIZATION-REVIEW.md:263` 说扫描器已单趟字节级 ⇒ 此项**仅为残余** |

### 3.2 改造

1. 用已解析的 manifest 结果做 `current`/`default` 等价判定。
2. 加宽 work-dir 标签。
3. 扫描器仅在实测显示为热点时才动。

### 3.3 风险

低。

---

## 4. 不要动

- `src/fnv.rs`、`ledger.rs`：事件溯源台账（write-only evidence）设计正确
- `guard.rs` 的语义守卫（决定"可推荐什么"）——这是本项目区别于纯 size reporter 的核心价值
- `budget.rs` 的 `pct_delta`（第 1 节要复用）

---

## 5. 验证与验收

### 5.1 现有测试面

- `tests/cli.rs`：CLI 行为与漂移容忍
- fixture：README 展示的 tiny fixture；`/tmp/rustopt-fixture-tiny.*` 模式

### 5.2 验收口径

| 改造 | 量化口径 | 正确性门禁 |
|---|---|---|
| tie band | 推荐决策在 band 内的并列率 | README 的 64 字节 fixture 必须输出并列 |
| 交错采样 | build time 中位数 + 区间宽度 | 重复运行结论稳定（同 fixture 同结论） |
| 等价检查 | 构建次数（5–8 → 少 1） | 有 `[profile.release]` 时 `current`/`default` 仍分别构建 |
| 结构化搜索 | 构建次数、试过的格点数、剪枝数 | fixture 上结果不差于穷举 |
| 并发 | 墙钟 | **size 逐字节与串行一致** |

### 5.3 负向验证

把 band 设为 0，README 的 fixture 必须回到"给出推荐"（证明 band 真的生效）；再把 band 设为 10%，必须输出全部并列（证明不是恒真）。

---

## 6. 拒绝清单

| 想法 | 为什么不做 | 来源 |
|---|---|---|
| 穷举 ~7.7e3 个 profile 组合 | 每次构建最慢数秒到数分钟 ⇒ 时间不可接受；且组合空间上的"最优"受构建噪声与测量模型限制 | 本设计 §2 |
| 用 bin packing / 配置 LP 的方法求"最优旋钮组合" | 配置 LP 间隙**无界**，加性常数内近似 **NP-hard** ⇒ 不要在这类组合空间上追求最优 | 结果 118 |
| 用"三机调度多项式算法"这类固定并行度结果推导构建调度 | 论文指数为 150020，自述 no practical running-time claim | 结果 124 |
| 把构建时间也当确定性量（单次采样就下结论） | 实测显示位置与变体混淆；`tests/cli.rs` 自己容忍 0.5% 漂移 | 本设计 §1 |
| 用加速编译（DFT/矩阵乘法类渐近结果）降低构建成本 | 常数荒谬（`δ = 10^-13`、`ω ≤ 9/4`） | 结果 107/130 |

---

## 7. 参考

- 完整映射报告：`dsfolder/MATH-TO-CODE-OPTIMIZATION-RESEARCH-2026-10-07.md`
- 原始调研证据（含逐字引用与 CONFIRMED/INFERRED 分级）：`dsfolder/HOTPATH-SURVEY-RUSTOPT-AND-COMFYUI-2026-10-07.md`
- 审计方法：skill `algorithmic-hotpath-audit`
- 决策纪律：`~/.claude/rules/algorithmic-hotpath-discipline.md`
- 本仓库既有文档：`docs/SELF-OPTIMIZATION-REVIEW.md`、`docs/optimization-space.md`、`docs/FINDINGS.md`、`docs/bottleneck-verified.md`
