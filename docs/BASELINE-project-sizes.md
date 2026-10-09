# 项目体积基线（全机 / 两机）

记录 **2026-10-07** · M1（`Echers-Mbp`，Apple M1 Pro，10 核）· M3（`losdeMacBook-Pro`，Apple M3 Pro）
口径：`du`（按 inode 去重）。`target/` 里 cargo 大量使用硬链接，逻辑字节会把同一份数据数很多遍（实测虚高可达 1.4×），**判断"删了释放多少"只用 du**。
相关：cantool 的详细基线见 `cantool/docs/build-size-baseline.md`；方法与判定见 [FINDINGS.md](FINDINGS.md)、[DESIGN-build-cache-governance.md](DESIGN-build-cache-governance.md)。

---

## 1. M1：各仓 target 体积与可回收量

| 仓 | target（清理后实测，2026-10-07） | 可回收（预览） | 相对配置前 | 本次动作 |
|---|---|---|---|---|
| `cantool/src-tauri` | **已删除**（清理前 11,012 MB：debug；incremental 4,669 + deps 6,450 + build 246） | — | −10.75 GiB | **已清理**（项目 12 G→1.2 G，台账 11,547,389,952 B） |
| `cankey` | 7.82 GiB（debug/deps 6.6 G） | 7.82 GiB | +0.18 | 仅加配置 |
| `canpad` | 4.92 GiB（debug/incremental 602 M） | 4.92 GiB | +0.28 | **加配置**（原先没关 incremental） |
| `dsfolder/unirun` | 2.74 GiB | 2.74 GiB | +0.15 | 仅加配置 |
| `dsfolder/verify-gate` | 0.44 GiB | 0.44 GiB | 0.00 | 仅加配置（+`strip="none"`） |
| `dsfolder/session-index` | 0.33 GiB | 0.33 GiB | 0.00 | 仅加配置 |
| `dsfolder/rustopt` | 0.25 GiB | 0.25 GiB（无清理脚本，by design） | 0.00 | 仅加配置 |
| `dsfolder/sandbox-run` | 0.25 GiB | 0.25 GiB | 0.00 | 仅加配置 |
| `dsfolder/run-diff` | 0.12 GiB | 0.12 GiB | 0.00 | 仅加配置 |
| `dsfolder/fmtguard` | 0.10 GiB | 0.10 GiB | 0.00 | 仅加配置 |
| `qsv2flv` | 0.09 GiB | 0.09 GiB | 0.00 | **加配置**（原先没关 incremental） |
| **小计（不含 cantool）** | **17.06 GiB** | **17.06 GiB** | **+0.61** | — |
| **合计（含 cantool 清理前）** | — | **≈ 27.8 GiB**（其中 10.75 GiB 已释放） | — | — |

> **"+0.61 GiB"是预期现象，值得记住**：改变 `[profile.dev.package."*"]` 会改变依赖的 unit hash，
> 于是**新旧两代产物在同一 target 树里并存**，cargo 不会回收旧的那一代（同一现象早前在
> verify-gate 上实测为 782 M → 990 M）。因此"改配置省磁盘"必须配一次清理才算数——这正是
> `clean-build-artifacts.sh --apply` 的用途。上表的"可回收"列就是现在清一次能拿回的完整量。

> 未列入：`cantool-dep-ws`（cantool 的依赖工作区变体，未构建、无 target）、`espanso` / `cc-switch/src-tauri` / `phone-geo-lookup/src-tauri`（第三方 checkout，**刻意不动**）、`cankey.bak-*` 等备份目录。

## 2. M3：各仓 target 体积与可回收量

| 仓 | target | 可回收 | 本次动作 |
|---|---|---|---|
| `cantool/src-tauri` | **2,540 MB**（release；deps 2,124 + build 335） | 2,540 MB | **已清理**（→ 项目 3.2 G→754 M，台账 2,663,407,616 B） |
| `cankey` | 2.31 GiB | 2.31 GiB | **加配置**（M3 侧） |

两机合计本轮释放：**11,547,389,952 + 2,663,407,616 = 14,210,797,568 B ≈ 13.23 GiB**。
（M1 只剩 debug 树、M3 只剩 release 树——符合"开发在 M1 / 出包在 M3"的分工；任一侧出现另一侧的树就说明构建跑错了机器。）

---

## 3. 本次配置变更登记（fleet profile 策略）

策略（逐条有实测依据，见 FINDINGS §2）：
```toml
[profile.dev]
incremental = false                # 不再长 incremental；代价是单 crate 重编 0.29 s → 0.69 s

[profile.dev.package."*"]
debug = 0                          # 依赖不带调试信息：dev target 372 MB → 225 MB（−40%），冷编 −12%
strip = "none"                     # `debug = 0` 会让 cargo 传 `-C strip=debuginfo`（实测 98 处），
                                   # 而 Apple strip 会破坏 proc-macro dylib（cantool 记录的坑）
```

| 仓 | `incremental=false` | deps `debug=0`+`strip=none` | `rust-toolchain.toml` | 备注 |
|---|---|---|---|---|
| `canpad` | **新增** | **新增** | 无 | 原先两项都没有 |
| `qsv2flv` | **新增** | **新增** | 无 | 原先两项都没有 |
| `cankey`（M1+M3） | 已有 | **新增** | 有（M3 侧为 1.97.0） | 6.6 G debug/deps 是最大单体依赖树 |
| `dsfolder/unirun` | 已有 | **新增** | 有 | — |
| `dsfolder/verify-gate` | 已有 | 已有 `debug=0` → **补 `strip="none"`** | 无 | 补保险丝 |
| `dsfolder/fmtguard` `run-diff` `sandbox-run` `session-index` | 已有 | **新增** | 无 | — |
| `dsfolder/rustopt` | 已有 | **新增** | 无 | **只放配置脚本，不放清理脚本**（README 明确 "No `target/` garbage collection"，且它是已发布 crate） |
| `cantool` | **刻意不改** | 已有（`opt-level=1`/`debug=false`/`strip="none"`） | 有（1.95） | `apply-build-profile.sh` 判定 `needs-review`（exit 3）以保护其调优；理由见 `cantool/docs/build-size-baseline.md` §4 |
| `cantool-dep-ws` | 不动 | 已有 | 有 | cantool 的变体，同策略 |
| `espanso` `cc-switch` `phone-geo-lookup` | 不动 | 不动 | — | 第三方 checkout |

**验证（全部重建后的构建/测试，2026-10-07）**：

| 仓 | 模式 | 重编 crate | 结果 | 耗时 |
|---|---|---|---|---|
| `dsfolder/rustopt` | test | 12 | ✓ 2 组 | 9 s |
| `dsfolder/fmtguard` | test | 13 | ✓ 1 组 | 3 s |
| `dsfolder/run-diff` | test | 22 | ✓ 1 组 | 2 s |
| `dsfolder/sandbox-run` | test | 22 | ✓ 1 组 | 4 s |
| `dsfolder/session-index` | test | 38 | ✓ 1 组 | 4 s |
| `dsfolder/verify-gate` | test | 86 | ✓ 1 组 | 9 s |
| `qsv2flv` | build | 15 | ✓ | 5 s |
| `dsfolder/unirun` | test | 60 | ✓ 10 组 | 17 s |
| `canpad` | build | 123 | ✓ | 21 s |
| `cankey` | build | 49 | ✓ | 61 s |

**10/10 通过。** 另：`cargo metadata --no-deps` 对 10 个仓全部解析通过。

**预期的一次性成本**：`[profile.dev.package."*"]` 改变了依赖的 unit hash ⇒ 每个仓的**下一次构建会全量重编依赖**（上表的"重编 crate"就是这次成本，已完成）。此后命中新 profile 的产物，不再重复付。

---

## 4. 脚本部署登记

| 脚本 | 位置 | 作用 |
|---|---|---|
| `clean-build-artifacts.sh` | `cantool`（规范副本）+ 9 个仓的 `scripts/` + M3 的 cantool/cankey | 清整个 `target`：du 前置分类报告、`target/<profile>/.cargo-lock` 的非阻塞 flock 闸门（fail-closed，exit 3）、两段式、JSONL 台账、`--json` 可做门禁、`--with-frontend` 可选 |
| `apply-build-profile.sh` | 同上 10 个仓 + M3 的 2 个仓 | 幂等写入上面的 profile 策略；**遇到已有自定义键只报告不覆盖**（cantool 因此安全） |

两机 sha256 一致性已校验（cantool、cankey 各 2 个文件）。

---

## 5. 明确没做的事

* 没有清理 `cankey`（M1 7.64 GiB / M3 2.31 GiB）、`canpad`（4.64 GiB）、`unirun`（2.59 GiB）等——用户只要求清 cantool；这些仓的 `clean-build-artifacts.sh` 已就位，随时可清（预览即可看到确切数字）。
* 没有删 `node_modules` / `dist` / `artifacts` / `.git` / `.jj`（见 cantool 基线文档 §1 的"保留"表）。
* 没有改 `cantool` 的 `[profile.dev] incremental`（刻意保留 dev 增量）；要止住那 4.6 GB 的再生，加一行即可，代价是单 crate 重编变慢。
* 没有动第三方 checkout（espanso / cc-switch / phone-geo-lookup）。
* 没有提交任何仓库（所有改动是工作区文件，见下节回滚）。

## 6. 回滚

| 改了什么 | 怎么回 |
|---|---|
| 10 个仓的 `Cargo.toml`（新增 `[profile.dev]` / `[profile.dev.package."*"]` 段） | 删掉新增的键/段；或在 git 仓里 `git checkout -- Cargo.toml`；`cantool-dep-ws`/第三方未改 |
| 10 个仓新增的 `scripts/*.sh` | 删除这两个文件即可（其它脚本未动） |
| M1/M3 cantool 的 `target/` | `cargo build` / M3 的 `release.sh` 重编（全量一次） |
| 全局 `~/.cargo/config.toml`（上一轮） | 删文件 |

## 7. 节奏建议

* **每批交付收尾**：`./scripts/clean-build-artifacts.sh`（预览）+ `./scripts/build-cache-sweep.sh --apply`（细档）；
* **每月**：对全部仓跑一次 `--json`，与本基线对照；单仓 target 超 12 GiB 报警；
* **换工具链 / 换 profile 后**：必定出现新旧产物并存（实测 cantool 782 M → 990 M 的同类现象），此时才需要 `--apply` 全清。

---

## 8. B1 实测：一次干净构建的稳态（2026-10-07，cantool 两机各跑一次）

| 机器 | profile / 命令 | 构建耗时 | **target（单次 du）** | 清理后 |
|---|---|---|---|---|
| M1 | dev · `cargo build --locked`（pinned 1.95） | 164 s | **4,150,176 KB = 3.96 GiB** | 项目 1.2 G（释放 3.96 GiB） |
| M3 | release · `./scripts/m3-build.sh`（Aqua 出包链路） | 6m09s（整链 6m39s） | **2,597,852 KB = 2.48 GiB** | 项目 755 M（释放 2.48 GiB） |

**累积 vs 稳态**（本轮最重要的修正）：

| 机器 | 首次清理前（累积值） | 一次构建（稳态 B1） | 倍数 |
|---|---|---|---|
| M1 dev | 10.75 GiB | **3.96 GiB** | **2.65×** |
| M3 release | 2.48 GiB | **2.48 GiB** | **0.98×** |

⇒ **清理前的"项目 12 G"里，target 的 10.75 GiB 是长期累积（旧 incremental 会话 + 旧世代），不是一次构建的真实需求；真实稳态是 3.96 GiB。** 这也回答了"哪个才是更准确的基线"：**一次干净构建后的数才是基线，累积值只能当"该清理了"的信号。**
（详细分解、以及"dev 多出的 1.48 GiB 里 72% 是 incremental、30% 是胖 debug 可执行文件、而 deps 两者几乎同大"这条反直觉结论，见 `cantool/docs/build-size-baseline.md` §8。）

**据此落地的回归门禁建议**：M1 dev `target` > **4.75 GiB**（1.2×）报警；M3 release `target` > **2.98 GiB** 报警；B2/B3 只记趋势。

**M3 持久化约束（本轮踩到的坑）**：`m3-build.sh` 在 M3 上执行 `jj new main`，会**移除未提交的工作副本文件**——实测我 scp 过去的脚本与本文档在构建后都不见了。因此 M3 上要么**提交进仓库**（推荐，走 forgejo/PR 流程），要么放在仓外（已把两个脚本放在 `~/bin/`，并给它们加了 `--repo DIR` 参数）。

---

## 9. 第三轮清理（2026-10-07）+ 提交登记

### 9.1 清理结果（用户确认的 cankey / canpad / unirun + M3 cankey）

| 仓 | 机器 | 清理前项目 | 清理后项目 | 释放（台账精确值） |
|---|---|---|---|---|
| `cankey` | M1 | 8,042 MiB | 36 M | 8,395,931,648 B = **7.82 GiB** |
| `canpad` | M1 | 5,247 MiB | 212 M | 5,279,760,384 B = **4.92 GiB** |
| `dsfolder/unirun` | M1 | 2,803 MiB | 1.2 M | 2,938,003,456 B = **2.74 GiB** |
| `cankey` | M3 | 2,395 MiB | 33 M | 2,476,867,584 B = **2.31 GiB** |
| **合计** | | **18,487 MiB** | **282 M** | **19,090,563,072 B = 17.78 GiB** |

### 9.2 累计释放（四轮操作）

| 轮次 | 内容 | 释放 |
|---|---|---|
| 1 | cantool M1 + M3（首次全清） | 13.23 GiB |
| 2 | cantool M1 + M3（构建后第二次全清） | 6.44 GiB |
| 3 | cankey/canpad/unirun(M1) + cankey(M3) | 17.78 GiB |
| **合计** | | **≈ 37.5 GiB** |

### 9.3 保留未清（有意）

M1 的 6 个小仓保留热缓存（刚验证过、重建成本虽小但没必要）：rustopt 251 M、fmtguard 105 M、run-diff 124 M、sandbox-run 256 M、session-index 333 M、verify-gate 445 M —— 合计 **1.48 GiB**。`artifacts/`、`.git`、`.jj`、`node_modules` 未动。

### 9.4 提交登记

| 仓 | 提交 | 内容 | 是否推送 |
|---|---|---|---|
| **cantool** | `1f59341d5b6d` + `165bda5c997f` | 清理脚本、配置脚本、体积基线文档（+ 文档更新） | **已推 forgejo + origin 的 main**（镜像一致，M3 实测 fetch 后文件齐全） |
| fmtguard | `b4058ff` | Cargo.toml（deps profile）+ 两个脚本 | 本地提交，未推 |
| sandbox-run | `743a5fb` | 同上 | 本地提交，未推 |
| unirun | `d9de886` | 同上 | 本地提交，未推 |
| verify-gate | `423a9f7` | 同上 | 本地提交，未推 |
| cankey | `0a25024` | 同上 | 本地提交，未推 |
| canpad | `cfbce01` | 同上 | 本地提交，未推 |
| rustopt | `5381c3e` | Cargo.toml + 配置脚本 | 本地提交，未推 |

**刻意未提交**：

* `dsfolder/run-diff`、`dsfolder/session-index`：它们在**共享父仓 `dsfolder`**（91 个待提交项）里，不是独立仓；路径限定提交会把文件提交进那个共享仓，需要你确认后再做。
* `qsv2flv`：**不是 git 仓**，配置改动只存在于工作树；要版本化需先建仓。
* `rustopt/docs/`（本轮全部分析文档）：rustopt 是**已发布 crate**，`docs/` 会进 `.crate` 包；等你决定是否提交（可能需要 `exclude`）。
* `espanso` / `cc-switch` / `phone-geo-lookup`：第三方 checkout，未动。
