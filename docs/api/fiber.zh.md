# Fiber

**fiber** 是处于其生命周期中的单个组件实例。`cordis_core::fiber` 提供纯函数式的 `FiberMachine`
状态机 —— [supervisor](supervisor.zh.md) 在每一条 fiber 记录背后运行它 —— 以及描述 fiber 生命周期
状态、其目标（desired / target）状态与在途转换的那些类型。该状态机是*纯*的 —— 它没有 I/O，也没有
锁 —— 因此 supervisor 可以作为它的唯一写入者来串行驱动它。

该状态机对应论文 §4.1 的状态机（图 1）及其九条规则。`DesiredState`/`DesiredEpoch`/`EpochEntry`
类型编码了论文在**目标**视图（`target_n(γ)`）与**已提交**视图（`ω_n`）之间划出的关键区分：
`DesiredEpoch` 是 fiber *应当*依据其运行的解析，而 `CommittedView`（见
[service](service.zh.md)）才是它实际激活时所依据的那个。

## `FiberState`

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FiberState {
    Pending,    // not yet active
    Loading,    // a Load transition is in flight
    Active,     // loaded and running
    Unloading,  // an Unload transition is in flight
    Failed,     // a Load finished with an error
    Disposed,   // retired and removed
}
```

| 论文状态 | Rust |
|---|---|
| `Inactive` | `Pending` |
| `Reloading` | `Loading` |
| `Active` | `Active` |
| `Unloading` | `Unloading` |
| （failure 扩展） | `Failed`, `Disposed` |

## `DesiredState`, `DesiredEpoch`, `EpochEntry`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesiredState {
    Waiting,               // no runnable epoch
    Ready(DesiredEpoch),   // a runnable target epoch
    Retired,               // retire and dispose
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesiredEpoch {
    entries: Arc<[EpochEntry]>,
}
impl DesiredEpoch {
    pub fn from_resolution(resolution: &DependencyResolution) -> Option<Self>;
    pub fn entries(&self) -> &[EpochEntry];
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpochEntry {
    pub key: ProviderKey,
    pub provider: Option<FiberId>,
}
```

- 一个 `DesiredEpoch` 是*一次 load 尝试的有序提供者选择*。
- `DesiredEpoch::from_resolution` 仅在解析处于 **ready（就绪）** 状态时返回 `Some` —— 每个
  `ResolvedInject` 要么有提供者，要么是 `Requirement::Optional`。这就是定义 21 的 `σ ⊨ d`
  满足关系。
- `EpochEntry` 记录的是**提供者**（`Option<FiberId>`），绝不是值。这正是论文的“记录提供者而非
  值”：值相等的两个提供者仍是不同的 fiber，替换只取决于选择的是*哪个* fiber。
- `DesiredState` 是**最新的生命周期目标**；当有转换在运行时，对它的更改会被**合并（coalesce）**
  （见下文的 inertia 规则）。

## `FiberTransition` 与 `TransitionKind`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FiberTransition {
    pub fiber: FiberId,
    pub generation: u64,
    pub kind: TransitionKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransitionKind {
    Load { epoch: DesiredEpoch },
    Unload,
}
```

一个 `FiberTransition` 是*要在 supervisor 任务之外运行的工作*：也就是该 fiber、一个严格的
`generation` 和它的 kind。supervisor 把它交给 executor，executor 用同一个 `generation`
回报完成情况。

## `TransitionAdvance`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransitionAdvance {
    IgnoredStale,                          // wrong generation, or no active transition
    Settled,                               // reached a stable state
    Start(FiberTransition),                // chain into the next transition
}
```

把一次转换的*完成*应用到状态机所得的结果。

- `IgnoredStale` —— 该完成携带的 generation 与在途转换不匹配，因此它被丢弃，当前转换不受影响。
- `Settled` —— 状态机到达了一个稳定状态（`Active`、`Pending`、`Disposed` 或 `Failed`）。
- `Start(next)` —— 状态机链入了一次后续转换（例如一次 `Load` 完成了但目标已变，于是它链入一次
  `Unload`）。

## `FiberMachine`

```rust
#[derive(Clone, Debug)]
pub struct FiberMachine { /* opaque */ }

impl FiberMachine {
    pub fn new(fiber: FiberId) -> Self;
    pub const fn state(&self) -> FiberState;
    pub fn desired(&self) -> &DesiredState;
    pub fn active_transition(&self) -> Option<&FiberTransition>;
    pub fn failure(&self) -> Option<&CordisError>;
    pub fn teardown_error(&self) -> Option<&CordisError>;

    pub fn set_desired(&mut self, desired: DesiredState) -> Option<FiberTransition>;
    pub fn complete(&mut self, generation: u64, result: Result<(), CordisError>) -> TransitionAdvance;
    pub fn restart(&mut self) -> Option<FiberTransition>;
    pub fn reload(&mut self) -> Option<FiberTransition>;
}
```

### `set_desired`

更新目标，并且**仅在状态机空闲时才开始工作**。如果有转换在途（`active` 为 `Some`），它只记录
`desired` 并返回 `None` —— 该更改会在当前转换完成时被采纳（合并）。否则：

- `Pending` + `Ready(epoch)` → 启动一次 `Load { epoch }`（状态 → `Loading`）。
- `Pending`/`Failed` + `Retired` → 状态 → `Disposed`，返回 `None`。
- `Active` + `Ready(same_epoch)`（已加载的 epoch 已经等于它）→ `None`，无工作。
- `Active` + 其它任何情况 → 启动一次 `Unload`。

### `complete`

完成一个在外部执行的转换，按 `generation` 匹配。过期的 generation 返回 `IgnoredStale`，状态机
不受影响。在一次真正的完成时：

- 带 `Ok(())` 的 `Load`：记录该 epoch；如果 desired 仍是带*相同* epoch 的 `Ready`，状态 →
  `Active`（对应算法 5 的 `if fiber.target = target0 then ACTIVE`）；否则它**链入**一次 `Unload`
  （目标在转换中途改变了）。带 `Err` 的 `Load` → 状态 → `Failed`，记录 `failure`。
- 一次 `Unload`：记录任何拆除（teardown）错误；然后 `Waiting` → `Pending`，`Ready(epoch)` →
  **链入**一次全新的 `Load`，或 `Retired` → `Disposed`。

### `restart`

根据最新的 desired 状态显式重试一个 **failed（已失败）** 的 fiber。仅在 `state == Failed` 时
生效；清除 `failure`。`Waiting` → `Pending`；`Ready(epoch)` → 启动一次 `Load`；`Retired` →
`Disposed`。

### `reload`

强制一个 **active** 的 fiber 先经历 `unload`，再对它 desired 的 epoch 做一次全新的 `load`。仅当
没有在途转换、状态为 `Active` 且 desired 为 `Ready` 时才启动 `Unload`。与 `restart` 不同，它不会
重试一个失败的 fiber。

## inertia 规则（转换的合并与链式）

转换一旦开始就会完成 —— 这就是论文的 inertia（§4.4）。它有两个推论：

1. **转换期间的目标更改会被合并。** 当 `active.is_some()` 时调用 `set_desired` 只记录新目标并返回
   `None`；不会启动第二个转换。该更改会在在途转换完成时被观察到。
2. **目标更改是链式而非打断。** 当一次 `Load` 完成、而目标不再是已加载的那个 epoch 时，`complete`
   返回 `Start(Unload)`，supervisor 下一步就运行它；而那次 `Unload` 又可能为新的 epoch
   `Start(Load)`。这条链一次只驱动一个转换；状态机绝不会在转换中途中止它。

该状态机由一个生成转换序列的测试（`generated_transition_sequences_preserve_state_invariants`）
证明：它驱动 128 个随机调度 × 256 步，并断言这个不变量 —— 状态机存在在途转换**当且仅当（iff）**
它是 `Loading` 或 `Unloading`，并且一旦 `Disposed` 就保持 `Disposed`。

## 提供者身份，而非值

`set_desired` 通过每条 entry 选择的是*哪个提供者 fiber* 来比较 epoch，而不是提供者持有的任何值。
`FiberId` 全新且永不复用，因此被替换的提供者绝不会与它的前任混淆 —— 即使它们提供的服务比较起来
相等（见 `fiber.rs:395-397`）。

## 错误

状态机本身不返回 `CordisError`；它在状态内部记录 `failure`（失败的 load）与 `teardown_error`
（失败的 unload），并通过访问器暴露它们。supervisor 再把它们转成 fiber snapshot 的
`failure`/`teardown_error` 字段。

## 深入参考

该状态机有专门的一页 —— [fiber-machine](fiber-machine.zh.md) —— 覆盖 Fibonacci 风格的穷举不变量
测试、精确的合并协议，以及完整的 load/unload 完成矩阵。
