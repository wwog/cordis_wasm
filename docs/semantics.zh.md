# 理论→实现对照表

本文档是论文 *A Programming Paradigm for Spatiotemporal Composability*
（arXiv:2608.25512，仓库根目录 `2608.25512v1.txt`）与本 Rust 实现的权威对照。论文形式化了
Cordis；本仓库是它的 Rust + Wasmtime 重写。两者不二进制兼容，也从未承诺——重写保留的是
**语义**（第 3 节的形式模型与第 4 节的演算），而非 TypeScript 的对象机制。

下表每一行给出论文构造、本仓库的运行时对应物、以及证据（`file:line` 或证明它的测试）。
实现有意识偏离的地方会注明。标记为"故意"的是工程决策，已记录在此与 README，不是模型的缺失。

---

## 1. 可逆 effect（论文 §3.1）

| 论文构造 | 实现 | 证据 |
|---|---|---|
| `𝔈Γ ≔ Γ → Γ × (Γ → Γ)` — 一个 effect 返回状态加逆 | `Disposer`（返回 future 的闭包）+ `EffectScope::defer` | `cordis-core/src/effect.rs:15-44`、`effect.rs:325-332` |
| `trackΓ` — 把逆合成到累积器上 | `EffectScope::defer` 把 disposer push 进 `Vec` | `effect.rs:326-332` |
| `recoverΓ` — 应用累积器，LIFO | `run_disposers` 用 `disposers.into_iter().rev()` 逆序执行 | `effect.rs:413` |
| effect 迭代器 `ℑΓ`（§3.1.3，定义 17/18） | `EffectGuard::spawn_stream` — stream 每个 item 是一个逆 | `effect.rs:160-195` |
| LIFO 恢复（定理 16） | `disposers_run_in_lifo_order` 测试 | `effect.rs:493` |
| 恢复越过失败的逆继续（定理 16） | `failures_do_not_skip_remaining_disposers` 测试 | `effect.rs:540` |
| 至多一次析构（论文 §5.1.1："触发两次会把逆应用在没有任何应用产生的状态上"） | `EffectGuard::dispose` 恰好一次状态机（`Armed/Draining/Disposing/Disposed`） | `effect.rs:229-267`；`dispose_is_idempotent` 测试 `effect.rs:477` |
| effect 归属到当前组件 | guest 的 `provide`/`listen` 把 disposer 落进当前 fiber 的 `EffectSet`；host 是清理的最终权威 | `dynamic.rs:209`（`InstanceHost::register`）、`runtime.rs:375`（`force_cleanup`） |

**witness 不校验。** 论文明确 `ctx.effect` 不检验返回的逆是否真的还原了 effect（`𝔈*`
witness）："逆是否还原了它伴随的 effect，是组件作者的责任，而非运行时验证的性质"（§5.1.1）。
本 Rust 实现同样不校验 witness —— `run_disposers` 执行逆，但无法确认它恢复了初始状态。两者
设计一致。

---

## 2. 响应式 coeffect（论文 §3.2）

| 论文构造 | 实现 | 证据 |
|---|---|---|
| coeffect context `Σ = (k : K) ⇀ V k` | `SupervisorState::providers: BTreeMap<ProviderKey, FiberId>` | `cordis-core/src/supervisor.rs:157` |
| `get(k)` | `resolve_dependencies` 读 `state.providers` | `supervisor.rs:893-913` |
| `set(k,v)` 带前置条件 `k ∉ dom(σ)` | `provide` 拒绝 `DuplicateProvider` | `supervisor.rs:748-752` |
| `withdraw`（`set` 的逆） | `withdraw` 校验 owner，移除槽位 | `supervisor.rs:765-791` |
| 满意度 `σ ⊨ d`（定义 21） | `DependencyResolution::is_ready()` + `DesiredEpoch::from_resolution` | `cordis-core/src/service.rs:122`、`fiber.rs:29-32` |
| `notify_d` 分类 activating / deactivating / neutral | `recompute_affected` 只重新解析 realm 匹配被改 key 的消费者 | `supervisor.rs:915-946` |
| 组件只在依赖齐备时激活（定理 70） | `desired_state`：ready resolution → `Ready`，否则 `Waiting` | `supervisor.rs:1130-1132` |
| provision 是可逆 effect（论文 §3.2.1：`set` *就是* `𝔈*Σ`） | `provide` 返回 `RegistryChange`；WASM host 把 withdraw 作为 `Disposer` defer | `loader.rs:223-244` |

**隔离与拦截。** 论文 §3.2.3 定义 `Σiso = (K ⇀ R) × ((r:R) ⇀ V r)` 与
`Σinter = ((k:K) → M k) × ((k:K) ⇀ (M k → V k))`，两者都由**派生 context**（定义 23）实现：它们
产生一个全新 context，恢复即丢弃，没有需跟踪的 effect。

- `ContextNode { parent, fiber, realms, intercepts }` 是递归 Γ 结构 — `cordis-core/src/context.rs:12-18`。
- `isolate(service, realm)` 返回新 overlay，父节点不动 — `context.rs:44-50`，对应论文实现方式（定义 25）。
- `intercept(service, value)` 返回新 overlay；`intercept_layers` 从外到内遍历 — `context.rs:54-60`、`76-83`。
- realm 解析是两层 `k ↦ ρ(k) ↦ σ(ρ(k))`：`resolve_realm` 走祖先链，再 `ProviderKey::new(service, context.resolve_realm(service)?)` — `context.rs:67-73`、`supervisor.rs:900-904`。

---

## 3. Context Paradigm（论文 §3.3）

`Γ∞ = μΓ. Γ × (Γ → Γ) × Σ` 把 effect context 与 coeffect context 统一成一个递归类型，其层级
支持嵌套组件（§3.3.1 的"插拔"隐喻）。

| 论文构造 | 实现 | 证据 |
|---|---|---|
| `Γ∞` 递归 context | `ContextNode { parent, fiber, realms, intercepts }` | `context.rs:12-18` |
| 派生子 context（定义 23） | `Context::extend` — 绑到子 fiber 的不可变 overlay | `context.rs:38-40` |
| 分层组合 | `InstanceHost` 携带 `Context`；`mount_dynamic` 扩展父 context | `dynamic.rs:150-164`、`dynamic.rs:570-571` |

**`≃`（观察等价）—— 唯一未形式化的点。** 论文 §3.3.2 与 §4.3.2 把恢复保证读作"up to `≃`"：
free 分配器不回绕、已发出的消息保持已发出。本实现不携带 type-level `≃`；它用结构手段达到同等
效果——外部不可逆资源（日志、已发出消息、跨进程 I/O）永远不会被"恢复"，因为它们的**获取**
（一个 `Disposer`）与**发出**（一次出站调用）是分离的，而发出就是论文的"acts as `idΓ`"。
语义上保证成立，但代码里没有形式化的 `≃` 类型。

---

## 4. 演算（论文 §4）

### 4.1 Fiber 状态机

| 论文状态（§4.1，图 1） | Rust `FiberState` |
|---|---|
| `Inactive` | `Pending` |
| `Reloading` | `Loading` |
| `Active` | `Active` |
| `Unloading` | `Unloading` |
| （failure 扩展，§4.4） | `Failed`、`Disposed` |

### 4.2 九条规则

| 规则 | 实现 | 证据 |
|---|---|---|
| `O-Insert` | `CreateFiber` 命令 | `supervisor.rs:562-578`、`create_fiber` `supervisor.rs:636` |
| `O-Retire` | `RetireFiber` 命令 | `supervisor.rs:839-852` |
| `O-Remove` | fiber 在 `Disposed` 后从 `state.fibers` 移除 | `supervisor.rs:656-677` |
| `L-Begin` | `set_desired(Ready)` 启动 `Load` | `fiber.rs:132-136` |
| `L-Iter` / `L-Finish` | `run_dynamic_transition` `Load` 分支执行 `activate` | `dynamic.rs:631-672` |
| `L-Leave` | `set_desired(Unload)` 启动 `Unload` | `fiber.rs:146` |
| `L-Divert` | `reload` 链（`replace` → `reload_fiber`） | `fiber.rs:194-203`、`dynamic.rs:435-474` |
| `L-Unload`（带 guard） | `schedule_transition_batch` 阻塞 unload，无消费者时释放 | `supervisor.rs:1068-1086`、`release_ready_unloads` `supervisor.rs:1104-1115` |

### 4.3 guard（空间可组合性的核心）

论文 §4.2.2：`relied_n(γ)` 在某已安装 fiber 把某 key 解析到 `n` 时成立；`L-Unload` 要求
`¬relied_n(γ)`。这就是提供者只在依赖者离开**之后**才撤销（定理 70）的原因。

- `has_active_consumers` 就是 `relied_n`：它测试是否有 `Loading/Active/Unloading` 状态的 fiber
  的 committed view 命名了该提供者 — `supervisor.rs:1117-1128`。
- `release_ready_unloads` 就是 guard：blocked unload 只在 `!has_active_consumers` 时释放 —
  `supervisor.rs:1104-1115`。
- 析构顺序消费者优先 → `teardown_drains_consumers_before_providers` 测试：
  `provider -> middle -> leaf`；retire provider 先拆除 `leaf`，再 `middle`，最后 `provider` —
  `supervisor.rs:1453`。

### 4.4 目标视图 vs 已提交视图

论文区分 `ω_n`（committed view，fiber 激活所依据的解析）与 `target_n(γ)`（它*应当*运行的
解析）；二者不同才触发转换。

- `desired`（`DesiredEpoch`）是目标视图；`committed`（`CommittedView`）是 `ω_n` —
  `supervisor.rs:698-708`、`commit_dependencies` `supervisor.rs:727-738`。
- 视图记录的是**提供者**（`EpochEntry { key, provider: Option<FiberId> }`，`fiber.rs:17-20`），
  而非值 —— 正是论文"记录提供者而非值"。
- 提供者身份是 `FiberId`，全新且永不复用，所以被替换的提供者不会与前任混淆，即使两者值相等
  （`fiber.rs:395-397`）。

### 4.5 依赖环

论文 §6.5："a dependency cycle simply leaves the involved components permanently inactive...
this condition is predictable from the dependency declarations alone, so a runtime can report it."

- `dependency_cycles` 对 provider–consumer 图做 SCC，把每个环成员置为 `Waiting` 并记录
  `DependencyCycle` 错误 — `supervisor.rs:979-1059`、`963-967`。
- 测试 `dependency_cycle_reports_every_scc_member` — `supervisor.rs:1404`。

---

## 5. 实现（论文 §5）

### 5.1.3 组件生命周期（算法 5）

`refresh` → 重算目标；若不在转换中，启动 `reload` 或 `unload`。`reload` 提交视图、执行 effect
函数、在完成时核对目标：若仍匹配，进入 `Active`；否则链入 `unload`。`unload` 逆序还原 effect、
等待依赖者（guard），然后 `Inactive` 或链入 `reload`。

| 算法 5 行 | 实现 |
|---|---|
| `fiber.target ← target` | `record.snapshot.desired` 由 `configure_dependencies` / `recompute_affected` 更新 |
| `fiber.committed ← resolve(inject)` | `commit_dependencies` 冻结 ready resolution — `supervisor.rs:727-738` |
| `recover ← await execute(fiber.apply, guard)` | `run_dynamic_transition` `Load` 分支执行 `activate` — `dynamic.rs:631-672` |
| `if fiber.target = target0 then ACTIVE else UNLOADING` | `complete_load` 检查 desired epoch 是否当前，否则链 `Unload` — `fiber.rs:205-225` |
| `unload ... await all(notify(...))` | blocked unload 等待消费者 — `supervisor.rs:1068-1086` |
| inertia（转换一旦开始会完成） | `FiberMachine` 把转换执行到底；转换中的目标变化会合并并链起 — `fiber.rs:125-149`、`fiber.rs:299-328` |

### 5.2.1 声明式配置与 reconcil

| 论文构造 | 实现 | 证据 |
|---|---|---|
| Entry 记录 `id, url, isolate, intercept, config, disabled`（§5.2.1，定义 81） | `EntrySpec { id, component, config, disabled, group, intercept, isolate, children }` | `cordis-loader/src/entry.rs:75-91` |
| 基于 `id` 的 keyed reconciliation | `EntryTree::reconcile`：stop（深度降序）、update、start | `entry.rs:224-289` |
| `@cordisjs/group` 嵌套加载 | `group: bool` + `children` | `entry.rs:117-128` |
| `@cordisjs/include` 外部 YAML/JSON | `cordis-loader/src/include.rs` | — |
| `isolate` local / global realm（论文算法 7） | `ManagedRealm::Local` / `Global`、`realm_for` | `entry.rs:131-135`、`loader.rs:514-535` |
| `intercept` 就地更新，无需 reload | `intercept` 元数据在读取时被查阅 | `context.rs:76-83` |
| 事务性（论文 §5.2.1：reconcile 由定理 80 保证可靠） | `rollback_error` 逆序撤销已应用操作 | `entry.rs:291-317`；测试 `failed_reconcile_rolls_back_applied_operations_in_reverse` |

### 5.2.2 热模块替换

| 阶段 | 实现 |
|---|---|
| 预检（候选编译 / descriptor / WIT / capability 检查*在触碰任何实例前*） | `reload_paths` 编译并检查每个候选，任何错误返回 preflight failure report — `hmr.rs:352-375` |
| 事务性替换 | `commit_candidates` 逐个替换；失败则对 `attempted.into_iter().rev()` 逆序回滚 — `hmr.rs:425-510` |
| backup / restore（论文算法 10） | `restore` 重新应用旧 artifact — `hmr.rs:216-218` |

**有意识差异。** 论文 HMR 分类的是*模块 import 图*（`get_imports`、`get_dependencies`，
Webpack/Vite 的 accept boundary）。这里动态代码是单个 Wasmtime Component，没有 JS 模块图：
HMR 简化为"artifact 内容 hash 变化 → 替换该 fiber"。语义核心保留——HMR 仍是 fiber 替换加
事务回滚，且仍无需开发者注记的 accept boundary（论文 §5.2.2 的中心论断），因为 fiber 已经
界定了组件的 effects。

---

## 6. 讨论（论文 §6）

| 论文节 | 实现 |
|---|---|
| §6.1 系统边界：获取（可逆）vs 发出（作为 `idΓ`） | WASM `provide_service`/`register_listener` 返回 host 持有的 `Resource<Registration>`（获取）；出站 `call_service` 的 payload 不被还原（发出）。`force_cleanup` 清空 registrations — `runtime.rs:375-385` |
| §6.2 服务复用：exclusive binding 或 broker | 通过 `DuplicateProvider` 实现 exclusive binding；*realms* 路线（论文的另一选项）通过 `ServiceId + RealmId` — `supervisor.rs:748` |
| §6.3 基于能力的访问控制 | 组件只能访问它声明的依赖：`call_service` 走 committed view，拒绝未声明/缺失提供者 — `loader.rs:195-205` |
| §6.3 沙箱 | Wasmtime Component Model：guest 只能通过 WIT kernel 接口到达 host；manifest 中的 `capabilities` 对照 `ArtifactPolicy` 校验 — `dynamic.rs:39`、`hmr.rs:33` |
| §6.4 语言无关 | 闭包（`Disposer`）、动态模块注册表（Wasmtime Component）、类型化依赖访问（`DependencySet` trait + `#[cordis::service]` 宏）、为 accessor 做编译期元编程（论文所述的 Rust 路径）— `native.rs:240` |
| §6.5 相互依赖 | SCC 环检测 — `supervisor.rs:979-1059` |
| §6.6 key 冲突 | `ServiceId` 携带 ABI hash（服务名 + 方法 + 参数类型 + 返回类型）；这是论文的 key-namespacing 路线 — `service.rs:8-31` |

---

## 7. 明确的、有意的偏离

下面这些不是模型的缺口；每个都是有意的工程决策，记录于 README 与本文件。

1. **Wasm HMR 没有模块 import 图。** 论文 HMR 遍历 JS import；重写替换单个 Wasmtime
   Component。核心语义（fiber 替换 + 事务）保留。
2. **业务服务协议是 Kernel WIT 之上的 MessagePack**，不是 TS 对象。README 声明"不承诺与 TS
   插件二进制兼容"。
3. **effect 归属的实现因路径而异。** native 路径用宏生成的 adapter；WASM 路径以 host 为最终
   权威（README 的"host effect 表是最终权威"）。
4. **`ReentrantCall` guard。** `CallGate` 拒绝同 fiber 重入，避免 Wasmtime Store 死锁
   （`dynamic.rs:325-348`）。论文讨论 inertia（§4.4）但不讨论重入。这是个限制性但必要的
   补充，记录在 `docs/wasmtime-findings.md`。
5. **effect witness 不做运行时校验** — 与论文一致（§5.1.1）；不是偏离。

---

## 8. 验证状态

上表每个语义机制在仓库里都有测试。下面把论文的核心定理映射到它们的测试：

| 论文定理 / 论断 | 测试 |
|---|---|
| 定理 16（LIFO 恢复） | `disposers_run_in_lifo_order` |
| 定理 70（ordering：提供者比消费者长寿） | `teardown_drains_consumers_before_providers` |
| 定理 73（progress / 无死锁） | `generated_transition_sequences_preserve_state_invariants` |
| §6.5（环可预测） | `dependency_cycle_reports_every_scc_member` |
| §5.2.1（事务性 reconcile） | `failed_reconcile_rolls_back_applied_operations_in_reverse` |
| §5.2.2（事务性 HMR） | `apply_failure_rolls_back_failed_and_prior_entries_in_reverse_order` |
| 定理 80（confluence：静默状态是最终配置的函数，与调度无关） | `quiescent_state_is_a_function_of_the_final_configuration_not_the_schedule` |
