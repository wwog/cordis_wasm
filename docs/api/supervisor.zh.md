# Supervisor 运行时

supervisor 是 Cordis 核心的**单写入者（single-writer）actor**。组件图、service/provider 表以及每个 fiber 的
生命周期状态，都只由一个任务修改，该任务按顺序处理命令。组件代码与事件回调运行在 supervisor *之外* — 一次
转换被交给一个 executor 任务，后者通过一条命令回报完成 — 因此用户代码永远不会运行在单写入者锁内部，也没有
任何东西会在 `await` 期间持有同步锁。

supervisor 是 JS 单线程事件循环的 Rust 替代物（README："Supervisor actor single-writer"）。它对应论文
§4 的规则：`CreateFiber` = `O-Insert`、`RetireFiber` = `O-Retire`、转换机制 =
`L-Begin`/`L-Iter`/`L-Finish`/`L-Leave`/`L-Divert`、unload 阻塞 = `L-Unload` guard、SCC 检测 = §6.5
依赖环。对应关系表见 [semantics.zh.md](../semantics.zh.md) §4。

## `Runtime`

```rust
#[derive(Debug)]
pub struct Runtime { /* handle: RuntimeHandle, supervisor: JoinHandle<()> */ }

impl Runtime {
    pub fn start() -> Self;
    pub fn handle(&self) -> RuntimeHandle;
    pub async fn shutdown(self) -> Result<RuntimeSnapshot, CordisError>;
}
```

持有单写入者 supervisor 任务。`start` 派生它（带 64 的命令缓冲区）；`handle` 克隆一个句柄；`shutdown` 发送
`Shutdown`，等待 supervisor 任务结束，并返回其最终快照。

**Panics** — 在 Tokio runtime 之外调用时，`start` 会 panic。

**Errors** — 若 supervisor 已关闭或其任务失败，`shutdown` 返回错误。

## `RuntimeHandle`

```rust
#[derive(Clone)]
pub struct RuntimeHandle { /* commands: mpsc::Sender<Command>, executors, changes */ }
```

runtime supervisor 的可克隆命令句柄。每个公开方法都会通过 channel 发送一条 `Command`，等待回复、通知等待者，
并派发任何已就绪的转换。

### 命令方法

```rust
pub async fn create_fiber(&self, parent: Option<FiberId>) -> Result<FiberId, CordisError>;
pub(crate) async fn create_live_child_fiber(&self, parent: FiberId) -> Result<FiberId, CordisError>;

pub async fn allocate_realm(&self) -> Result<RealmId, CordisError>;

pub async fn configure_dependencies(
    &self, fiber: FiberId, context: Context, injects: Vec<InjectSpec>,
) -> Result<DependencyChange, CordisError>;

pub async fn commit_dependencies(&self, fiber: FiberId) -> Result<CommittedView, CordisError>;

pub async fn provide(&self, key: ProviderKey, provider: FiberId) -> Result<RegistryChange, CordisError>;
pub async fn withdraw(&self, key: ProviderKey, provider: FiberId) -> Result<RegistryChange, CordisError>;

pub async fn complete_transition(
    &self, fiber: FiberId, generation: u64, result: Result<(), CordisError>,
) -> Result<TransitionUpdate, CordisError>;

pub async fn retire_fiber(&self, fiber: FiberId) -> Result<Vec<FiberTransition>, CordisError>;
pub async fn restart_fiber(&self, fiber: FiberId) -> Result<Vec<FiberTransition>, CordisError>;
pub async fn reload_fiber(&self, fiber: FiberId) -> Result<Vec<FiberTransition>, CordisError>;

pub async fn snapshot(&self) -> Result<RuntimeSnapshot, CordisError>;
pub async fn await_quiescent(&self) -> Result<RuntimeSnapshot, CordisError>;
```

**Errors everywhere** — shutdown 之后是 `CordisError::RuntimeClosed`，外加每条命令各自报告的具体错误
（参见各自方法上的 `# Errors` 文档）。

- `create_fiber(parent)` — 校验可选的父级之后创建 fiber。父级未知时返回 `UnknownFiber`。
- `create_live_child_fiber(parent)` — 在父级处于 `Loading` 或 `Active` 时创建子级。这是生成的方法级
  注入所使用的生命周期安全入口；父级不活跃时返回 `InactiveFiber`。
- `allocate_realm()` — 分配一个本进程内永不复用的 `RealmId`。
- `configure_dependencies(fiber, context, injects)` — 声明或替换 fiber 的依赖，并计算其期望解析。显式
  替换还会重试失败的 fiber。可能返回 `ContextFiberMismatch`、`UnknownFiber`、`DuplicateInject`、
  `MissingRealm` 或 `RuntimeClosed`。
- `commit_dependencies(fiber)` — 为一次 load epoch 冻结当前 ready 解析。必需 provider 缺失时返回
  `InactiveDependency`。
- `provide(key, provider)` — 占据一个 `(service, realm)` provider 槽位。槽位已被占用时返回
  `DuplicateProvider`。provider fiber 未知时同样会报 **Errors**。
- `withdraw(key, provider)` — 释放 `provider` 拥有的一个槽位。可能返回 `ProviderNotFound`、
  `ProviderOwnershipMismatch`。
- `complete_transition(fiber, generation, result)` — 报告工作完成（executor 正是以此告知 supervisor 一次
  `Load`/`Unload` 已经结束）。可能返回 `UnknownFiber`、`TransitionBlocked`（当 guard 阻塞 unload 时）
  或 `RuntimeClosed`。
- `retire_fiber(fiber)` — 把 fiber 标记为已退役，并在需要时返回清理工作。
- `restart_fiber(fiber)` — 针对其最新 desired epoch 显式重试失败的 fiber。
- `reload_fiber(fiber)` — 强制 active fiber 经历一次 unload 并对其 desired epoch 重新 load。与
  `restart_fiber` 不同，它不会重试失败的 fiber。
- `snapshot()` — 返回由 supervisor 产生的稳定快照。
- `await_quiescent()` — 等到没有 fiber 转换在途，然后返回稳定快照。等待缺失依赖的 fiber 被视为处于
  静止（quiescent）状态。

### 内部 helpers

`install_executor`、`remove_executor`、`await_disposed` 与 `await_settled` 均为 `pub(crate)`，供
dynamic/native 路径使用。

## 快照类型

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FiberSnapshot {
    pub id: FiberId,
    pub parent: Option<FiberId>,
    pub desired: DependencyResolution,
    pub committed: Option<CommittedView>,
    pub state: FiberState,
    pub active_transition: Option<FiberTransition>,
    pub dependency_error: Option<CordisError>,
    pub failure: Option<CordisError>,
    pub teardown_error: Option<CordisError>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSnapshot {
    pub fibers: Vec<FiberSnapshot>,
    pub allocated_realms: u64,
    pub provider_count: usize,
}
```

`FiberSnapshot` 是单个 fiber 的对外可观测视图：其 desired（target）解析、committed（ω_n）视图、状态、
任何在途转换，以及三个错误槽位。`RuntimeSnapshot` 则是整张图加上统计。

## `DependencyChange`、`RegistryChange`、`TransitionUpdate`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyChange {
    pub resolution: DependencyResolution,
    pub transitions: Vec<FiberTransition>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryChange {
    pub key: ProviderKey,
    pub affected: Vec<FiberId>,
    pub transitions: Vec<FiberTransition>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionUpdate {
    pub status: CompletionStatus,
    pub ready: Vec<FiberTransition>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionStatus {
    Applied,
    IgnoredStale,
}
```

- `DependencyChange` 是 `configure_dependencies` 的结果：新的解析，加上任何因此变为可运行的转换
  （例如现在可以启动的一次 load）。
- `RegistryChange` 是 provide/withdraw 的结果，已经收窄到受影响的消费者：被改变的 key、desired 解析
  发生变化的 fiber，以及需要派发的转换。
- `TransitionUpdate` 是 `complete_transition` 的结果：完成是被应用，还是作为过期（stale）而被忽略，
  外加那些已变为就绪的转换。

## Fiber 的生命周期

1. `create_fiber` 插入一个 `Pending` fiber。
2. `configure_dependencies` 赋予它一个 context 与注入；supervisor 的 `resolve_dependencies` 对照
   providers 表计算 `(service, realm)` key。若已就绪，fiber 进入 `Ready(epoch)` 并启动一次 `Load`。
3. `commit_dependencies` 把 ready 解析冻结进 `CommittedView`。
4. executor 执行 `Load`（激活组件）；`complete_transition` 回报结果。`complete_load` 将其设为 `Active`
   （若目标中途改变则链入 `Unload`）。
5. `provide`/`withdraw` 重新计算受影响的消费者；provider 出现会唤醒消费者，provider 离开则使它们停用。
6. `retire_fiber` 把 desired 设为 `Retired`，并最终设为 `Disposed`。

## Unload 守卫（空间可组合性）

论文的 `L-Unload` 规则要求 `¬relied_n(γ)`：provider 只有在它的依赖者都离开之后才能 unload（定理 70）。
supervisor 以**被阻塞的 unload（blocked unloads）**映射实现这一点。

- `schedule_transition_batch` 把每个 `Unload` 放进 `state.blocked_unloads`，暂不派发。
- `release_ready_unloads` 仅在 `!has_active_consumers` 时释放被阻塞的 unload。
- `has_active_consumers(provider)` 就是 `relied_n`：它测试是否存在某个 `Loading`/`Active`/`Unloading`
  fiber 拥有命名该 provider 的 **committed** 视图。

因此一次 unload 会先等待其消费者完全 unload — 消费者优先的拆除。测试
`teardown_drains_consumers_before_providers` 在 `provider -> middle -> leaf` 链上退役一个 provider，
并断言 `leaf`、接着 `middle`、最后 `provider` 依次被拆除。

## 依赖环检测

`dependency_cycles` 对 provider–consumer 图运行 SCC，并把每个环成员标记为 `Waiting`，附带
`CordisError::DependencyCycle { fibers }`。处于环中的组件永远不会激活；这种"永久不激活"的结果仅凭依赖
声明即可预知（§6.5）。自环也会被检测到（`graph[&fiber].contains(&fiber)`）。测试：
`dependency_cycle_reports_every_scc_member`。

## 错误

- `RuntimeClosed` — shutdown 之后的任何命令。
- `SupervisorFailed { message }` — supervisor 任务发生了 panic。
- `TransitionBlocked { fiber }` — 在 unload 被阻塞的 fiber 上完成转换。
- 外加上文各命令列出的具体错误。
