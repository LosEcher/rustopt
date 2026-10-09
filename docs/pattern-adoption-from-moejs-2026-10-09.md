# moejs 模式迁移：优化输入"生效性"与门禁骨架

**状态**：机制 8 **已实现**（2026-10-09）；机制 10 评估后**不立项**（落点下调，见 §2）。

- 日期：2026-10-09
- 来源：`dsfolder/MOEJS-PATTERN-TRANSFER-2026-10-09.md`（moejs @ `50811d9`，含逐条 `file:line` 与三档证据分级）
- 方法/纪律：技能 `transferable-pattern-audit`；规则 `~/.claude/rules/pattern-transfer-discipline.md`
- 相关既有文档：[pattern-adoption-from-shaders-2026-10-07.md](./pattern-adoption-from-shaders-2026-10-07.md)、[SELF-OPTIMIZATION-REVIEW.md](./SELF-OPTIMIZATION-REVIEW.md)

---

## 0. 为什么是"生效性"这一条

mo​ejs 是反例的现场：它随源码发布 `default.pgo`，README 还教你怎么传，
但 profile 内 **602 个符号的包路径是 `github.com/QuantumNous/moejs`**，而 `go.mod` 的 module 是
`github.com/Calcium-Ion/moejs` ⇒ **匹配符号 0 个**，整份 profile 对编译零贡献；Go 对"名字不匹配"
**既不报错也不警告**（只在 profile 打不开/解析失败时 fatal）。

对照本仓：**rustopt 自己的测量路径是健全的**——变体用 `cargo --config profile.<name>.<key>=<v>` 施加，
`variants::config_args`（`src/variants.rs:190-198`）的前缀由 `--build-profile` 推导，所以
`--build-profile dist` 也真的落在 `profile.dist` 上。风险不在"我们量错了"，而在**我们给的建议会被写到
一个不生效的地方**：`plan` 的 `profile` 字段是"要写进 `[profile.release]` 的键"，
但从未说明**写进哪个 manifest**。而 cargo **只从 workspace 根 manifest 读 profile**。

---

## 1. 机制 8（已实现）：建议必须点名"生效位置"，并且该规则被 cargo 自己的输出钉住

**实测本机 cargo（`cargo build --release -v`，2026-10-09）**，workspace 根为虚拟 manifest、
成员 manifest 里写 `[profile.release] strip = "symbols"`：

| 事实 | 观测 |
|---|---|
| 成员里的 profile | **被忽略**：rustc 收到的是 `-C strip=debuginfo`（cargo 默认），不是 `strip=symbols` |
| cargo 的提示 | stderr 出现 `warning: profiles for the non root package will be ignored, specify profiles at the workspace root` |
| 从根用 `--config profile.release.strip="symbols"` | rustc 收到 `-C strip=symbols` ⇒ **rustopt 的测量形式是有效的** |

结论：**cargo 会警告，但只在构建期 stderr 上**；`plan` 的消费方（人、CI、别的 agent）
拿到的是键，不是"写哪里"，而把它写进成员 manifest 不会让任何构建失败。

**改动**：
- `src/measure.rs`：`PkgMeta` 新增 `workspace_root` / `is_workspace_root`（取自
  `cargo metadata` 的 `workspace_root`，用**cargo 自己的视图**而不是目录猜测；两者任一无法解析时
  一律按"是根"处理——读了却读错的告警比没有告警更坏），并新增 `ProfileSite` 与
  `PkgMeta::profile_site(profile)`（给出 `package_manifest` / `effective_manifest` /
  `is_workspace_root` / 仅在非根时存在的 `warning`）。
- `src/plan.rs`：`Plan` 与 `CheckReport` 都带 `profile_site`；`plan` 在非根时额外产出
  `warn profile-site`（走既有 `findings` 通道 ⇒ 会进 `report.rs` 的 GUARDS 段与事件台账），
  `check` 把它写进 `notes`。

**机械验收（3 条，均在 `tests/cli.rs`）**：
1. `a_workspace_member_is_told_where_the_profile_must_be_written`：成员包 ⇒
   `profile_site.is_workspace_root == false`、`effective_manifest` 规范化后等于**根**的 `Cargo.toml`、
   且存在 `id == "profile-site"` 且 `severity == "warn"` 的 finding。
2. `a_standalone_package_gets_no_profile_site_warning`（**负向控制**）：独立包 ⇒
   `is_workspace_root == true`、`warning` 为 null、无 `profile-site` finding。
   没有这一臂，"永远告警"的实现也能过第一条。
3. `cargo_ignores_a_profile_declared_in_a_workspace_member`（**钉住被依赖的 cargo 规则**）：
   直接读 `cargo build -v` 的 stderr，断言①出现 cargo 自己的忽略警告②**不**出现 `-C strip=symbols`
   ③换成 `--config` 形式后**出现** `-C strip=symbols`。若未来 cargo 改为尊重成员 profile，
   这条红 ⇒ 改文案而不是留着过期结论。

---

## 2. 机制 10（评估后不立项）：`check`/`plan` 已经是"廉价门禁 / 昂贵门禁"的骨架

**原建议**：给编译产物打指纹，指纹没动就走廉价门禁，动了才升级到全套 A/B 测量。

**评估结论**：**本仓不需要新增机制**，原建议的落点判错了层。理由：
1. 骨架已在：`check` 只构建**一个**（你实际发布的）配置并与预算比对——这就是廉价门禁，
   而且舰队 `.rust-los-gov/gates.json` 用的正是 `check --build-profile dist`；
   `plan` 构建整张变体矩阵——这才是昂贵门禁，且**已经是显式 opt-in**（"run `rustopt plan` to see
   which measured variant would fit"）。也就是说"先廉价、后昂贵"不是缺失，而是本仓既有的分工。
2. 残余增量在**消费方**（CI/治理决定"这次改动要不要跑 `plan`"），不在 rustopt：
   要在本仓实现它，就得引入一个**新的状态文件**（产物指纹基线）与一条新命令，
   而目前**没有任何已证实的消费者**（`gates.json` 只跑 `check`）。按 ADR 0047 的边界判据，
   没有消费者证据的能力不应落在 owner 仓里。
3. 被击败的备选：把指纹做进 `check`（"指纹未变则跳过构建，直接复用上次字节数"）。
   **不采纳**：`check` 的语义是"量你**今天**发布的东西"，它已经在隔离 work dir 里重新构建，
   跳过构建会让 `check` 可能在陈旧产物上判 pass——用一个假绿通道换速度，正是 moejs 那份审计里
   被点名的反面模式。真要省时间，应省在**调用方**（别在无关改动上跑 `plan`），而不是让 `check` 相信旧字节。

**留给外部的判据（若将来 CI 要自动跳过 `plan`）**：指纹至少必须覆盖
①依赖闭包（`cargo metadata`，含 feature 解析）②编译选项/`--build-profile` ③参与该 target 构建的源文件集合；
且必须有一条负向控制证明"改了会进二进制的源文件 ⇒ 指纹变 ⇒ 强制跑 `plan`"。
