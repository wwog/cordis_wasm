# Fiber Machine 参考

更深入地看 `FiberMachine` 的不变量，以及把“碰巧能工作的状态机”与“可证明保持其不变量的状态机”
区分开来的那些行为。[fiber](fiber.zh.md) 覆盖公开接口（public surface）与语义；本页深入内部证明
与精确的链式协议。

这台状态机刻意地不靠自己完成全部工作：它只*记录*哪些工作在途（`active: Option<FiberTransition>`）
以及下一步该做什么工作（`TransitionAdvance::Start`）。[supervisor](supervisor.zh.md) 才是唯一的
写入者 —— 它真正运行转换，并通过 `complete` 回报。正是这种拆分 —— 纯决策状态机 + 外部 executor
—— 让不变量可以通过穷举来测试。

## 不变量

核心不变量，在生成测试的每一步都被断言：

```rust
let transitioning = matches!(machine.state(), FiberState::Loading | FiberState::Unloading);
assert_eq!(machine.active_transition().is_some(), transitioning);
if disposed { assert_eq!(machine.state(), FiberState::Disposed); }
```

用文字表述：

1. **存在在途转换当且仅当状态是 `Loading` 或 `Unloading`。** 状态机绝不会有悬空的 `active` 转换
   （否则 executor 会在一次完成上永远等下去），也绝不会在没有任何 `active` 记录的情况下报告一个
   暗示有工作在进行的生命周期状态。
2. **`Disposed` 是吸收态（absorbing）。** 一旦 `Disposed`，任何输入都不会让它离开。

这就是 `generated_transition_sequences_preserve_state_invariants`
（`fiber.rs:407-464`）中的 `transitioning` 断言。该测试驱动 128 个种子，每个 256 步，混合每一种
输入（把 `set_desired` 设为 `Waiting`/`Ready`/`Retired`、`restart`、`reload`、带当前 generation
与饱和 +1（saturating +1）generation 的 `complete`，以及 `Ok` 与 `Err` 两种结果），并在*每一步*
之后检查不变量。由于输入空间很小、状态空间也很小，这是对可达行为的一次穷举扫描。

## 转换合并

转换期间的目标更改不会启动第二个转换。`set_desired`：

```rust
pub fn set_desired(&mut self, desired: DesiredState) -> Option<FiberTransition> {
    self.desired = desired;
    if self.active.is_some() {
        return None;   // coalesce: remember the target, do nothing now
    }
    ...
}
```

supervisor 只在在途转换完成时才知道这次更改。这正是论文 inertia 背后的机制：你无法打断正在运行的
`Load`；你只能记录一个新目标，状态机会在下次 `complete` 时观察到它。

合并不是“以正确性为代价的最后一次写入胜出”：因为 `complete` 会拿*当前*的 desired 与*刚完成*的
epoch 重新比对，最终状态总是反映最新的目标，无论跳过了多少个中间目标。测试
`desired_changes_coalesce_during_load` 驱动 `Ready(first)` → `set_desired(Ready(second))`（得到
`None`）→ `complete(first, Ok)` → 期望一次链入的 `Unload` → `complete(unload, Ok)` → 期望一次链入
的 `Load(second)`。被跳过的 `first` epoch 从未被激活。

## 链式而非打断

当一个转换完成、而目标不再匹配时，状态机返回 `TransitionAdvance::Start(next)` 而不是安定下来。
两处链式点：

- **`complete_load`**：`is_current` 把 `desired` 与刚加载的 epoch 比较。若它们不同，它记录
  `loaded_epoch` 并返回 `Start(start(Unload))`。（然后那次 `Unload` 在自己的完成时重新检查同一个
  `desired`，如果它仍是 `Ready`，就链回一次 `Load`。）
- **`complete_unload`**：对 `Ready(epoch)` 返回 `Start(start(Load { epoch }))`；对 `Waiting` 安定
  到 `Pending`；对 `Retired` 安定到 `Disposed`。

因此，单个*复合*更改（例如在加载 `A` 时 `Ready(A)` → `Ready(B)`）会变成一次 `Unload` 链，随后接
一次 `Load(B)`。这条链一次始终只走一个转换 —— 状态机绝不会在一次调用中执行两个转换，也绝不会中止
一个转换去替换成新的。

## 代际计数器（generation counter）

每次 `start` 都会用 `next_generation` 给转换盖上时间戳并递增它。`complete` 忽略任何不等于在途
转换的 generation 的代数：

```rust
if active.generation != generation { return TransitionAdvance::IgnoredStale; }
```

这让迟到的完成变得无害。设想一下：一次 `Load` 完成，状态机链入一次 `Unload`，随后一个*过期的*
`Load` 完成（来自重复的报告，或一个延迟的任务）才到达。它是 `IgnoredStale` —— 它无法破坏 `Unload`
的在途转换，也无法改变状态。测试 `stale_completion_cannot_change_current_transition` 完成
generation + 1，并断言在途转换不变。

## load/unload 完成矩阵

| 已完成的转换 | 结果 | guard | 结局 |
|---|---|---|---|
| `Load { epoch }` | `Ok` | `desired == Ready(epoch)` | state → `Active`; `Settled` |
| `Load { epoch }` | `Ok` | `desired != Ready(epoch)` | record `loaded_epoch`; `Start(Unload)` |
| `Load { epoch }` | `Err` | — | `failure = Some(e)`; state → `Failed`; `Settled` |
| `Unload` | `Ok`/`Err` | `desired == Waiting` | record `teardown_error`; state → `Pending`; `Settled` |
| `Unload` | `Ok`/`Err` | `desired == Ready(epoch)` | record `teardown_error`; `Start(Load(epoch))` |
| `Unload` | `Ok`/`Err` | `desired == Retired` | record `teardown_error`; state → `Disposed`; `Settled` |

注意，一次 `Unload` 会记录错误，但**绝不会阻塞退役（retirement）**：当 fiber 被退役时，一次失败的
拆除仍会到达 `Disposed`，supervisor 会把它作为 `teardown_error` 呈现出来（测试：
`unload_failure_is_recorded_but_does_not_block_retirement`）。

## 与 `fiber.md` 的关系

本页是更深的参考。公开接口（public API）、`FiberState`/`DesiredState`/`EpochEntry` 类型以及
`restart`/`reload` 的时延（latency）都在 [fiber](fiber.zh.md) 中；supervisor 对 `TransitionAdvance`
的消费 —— 派发 executor、在消费者处阻塞 unload、以及环检测 —— 在 [supervisor](supervisor.zh.md)
中。
