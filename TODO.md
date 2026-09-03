# Cordis Rust implementation TODO

本文档是 `plan.md` 的执行清单。状态约定：`[ ]` 未开始、`[~]` 进行中、`[x]` 已完成、`[!]` 被验证结果阻塞。

## 实施规则

- 优先选择最小、直接、可测试的实现；没有明确收益时不增加抽象层。
- 状态变化必须有唯一入口，不用多个布尔值表达同一状态。
- 公共 API 不使用含义不明的 `bool` 参数，改用 enum 或具名 options。
- 不跨 `.await` 持有同步锁；不在 Supervisor 内执行用户代码。
- 优化必须由复杂度、内存或基准数据支撑；先保证语义，再加 fast path。
- 动态插件只走 Wasmtime Component Model；不同时维护 dylib 路线。
- 每完成一项同时补测试和文档，不积累“最后再测”的任务。

## Phase 0：风险验证与工程基线

- [x] 创建 Cargo workspace、Rust 2024/1.98 工具链配置和基础 lint。
- [x] 创建 CI、`cargo-deny`、`cargo-audit` 与依赖更新策略。
- [x] 把 TypeScript 测试行为整理成 `docs/parity.md`。
- [x] 冻结 Kernel WIT 0.1 草案并用 Wasmtime 48 bindgen 验证。
- [x] Spike：Component 加载、typed host import、guest export、Store drop。
- [x] Spike：WIT resource destructor 与强制 cleanup；结论是 Store drop 不会析构 guest 遗失句柄，host EffectGuard 必须是权威清理表。
- [x] Spike：fuel、epoch deadline、memory/resource limit。
- [x] Spike：waterfall 同 Store onion 重入与取消；可保留 one-shot `next()`，但禁止 drop in-flight call future，超时须合作取消或销毁 Store。
- [x] Spike：`wasm32-wasip2` guest 构建与 componentize 流程。

## Phase 1：cordis-core

### Effect

- [x] 定义无复用的 typed IDs 和公共错误类型。
- [x] 实现 exactly-once async disposer。
- [x] 并发 dispose 等待同一次清理并返回相同报告。
- [x] 实现 LIFO、错误聚合、panic 隔离和失败后继续清理。
- [x] 实现只能创建新子节点的嵌套 effect API 与 effect metadata tree。
- [x] 覆盖幂等、并发、顺序、错误、panic、inactive 注册测试。
- [x] 实现 effect stream 与 generation boundary 中断。
- [x] 实现 Fiber 顶层 EffectSet 和可观测快照。

### Context / Coeffect / Fiber

- [x] 实现 Runtime Supervisor 单写者命令循环（当前覆盖 ID、Fiber 层级与快照命令）。
- [x] 实现 Context immutable overlay、extend、isolate、intercept。
- [x] 实现 ServiceId/ABI hash、RealmId、provider store。
- [x] 实现 InjectSpec、committed view 和访问错误。
- [x] 实现 provide/withdraw 精确通知和 provider 唯一性。
- [x] 实现纯 Fiber 状态机、desired epoch、generation 与 inertial load/unload。
- [x] 把 Fiber 状态机接入 Supervisor；actor 只返回 transition work，外部执行后回报 generation。
- [x] 实现 consumer-first teardown、失败恢复、restart/update。
- [x] 实现依赖 SCC 诊断（自环和多 Fiber 强连通分量，环解除后自动恢复）。

### Events

- [x] 实现 effect-owned listener registry 与顺序 ID。
- [x] 实现 emit/parallel/serial/bail。
- [x] 实现 native waterfall 和 one-shot Next。
- [x] 实现 realm filter、global、prepend。

## Phase 2：宏与 native 组件体验

- [x] `#[cordis::service]`：规范化 ABI hash、typed client、native fast path 与动态 MessagePack dispatcher。
- [x] `#[cordis::event]`：EventId、稳定 ABI、MessagePack codec 与五种模式 typed dispatch helpers。
- [x] `#[cordis::component]` / `#[cordis::inject]`：descriptor、required inject、强类型依赖字段与 native apply adapter。
- [x] method-level inject child Fiber：同实例串行执行、独立依赖/EffectSet、provider 变更重载与父 effect 级联 retire。
- [x] config schema 生成与 `trybuild` compile-pass/fail 测试（固定 Draft 2020-12，并覆盖配置约束、未声明依赖、非 Send future、重复 inject 与非法宏组合）。
- [x] facade crate 和真实调用注入服务的 native counter 示例。

## Phase 3：Wasmtime host 与 guest SDK

- [x] `cordis-wasm` Engine/Linker/Store/ResourceTable 基础层。
- [x] Kernel WIT host/guest bindings（工作区单一 WIT 源）。
- [x] WASIp2 默认拒绝能力模型。
- [x] WASM ComponentFactory/ComponentInstance adapter，并通过 `mount_dynamic` 接入 Supervisor Fiber 生命周期。
- [x] 动态 service codec、ABI hash 校验和跨边界路由。
- [x] WASM event callback 与 waterfall 风险结论落地。
- [x] registration resource、Fiber 强制 cleanup、task shutdown。
- [x] guest SDK、guest macros、xtask 和双 WASM 示例。
- [x] trap/timeout/OOM/payload/capability 测试。

## Phase 4：Loader / Include

- [x] Entry Tree、Group、create/update/move/remove。
- [x] keyed reconcile 与 self-update/self-disable。
- [x] managed local/global realm 和精确通知。
- [x] JSON/YAML Include、patch、只读检测、原子写回。
- [x] 受限 Rhai `!expr`。
- [x] 移植 loader/include/isolate 行为测试。

## Phase 5：WASM HMR

- [x] notify watcher、debounce 和内容 hash 去重。
- [x] candidate compile/manifest/WIT/capability preflight。
- [x] batch prepare、commit、rollback，并通过 `FiberReloadRuntime` 执行真实 Fiber unload/load/restore。
- [x] compiled Component 缓存和 reload report。
- [x] 半写文件、坏 component、apply trap、rollback 失败测试。

## Phase 6：配套与发布

- [x] Timer timeout/interval/sleep、Logger/console exporter、tracked collections 基础能力。
- [x] Timer Stream/throttle/debounce；Logger 经 API review 明确为与 `tracing` 平行的服务，不安装反向 subscriber。
- [x] Loader → Wasmtime → Supervisor → HMR 运行时装配与声明式双 guest 示例。
- [x] `cordis run/check/inspect/build-component`。
- [~] 属性测试、故障注入、Miri/Loom、跨平台 CI（属性/故障/Miri 本地通过，Loom 经审计不适用；待远端跨平台结果）。
- [x] Fiber 状态机确定性生成序列不变式测试（128 seeds × 256 operations）。
- [x] MSRV 1.98、Linux/macOS/Windows、fmt/Clippy/rustdoc/真实 WASM E2E CI 基线。
- [x] 建立 lifecycle/Context benchmark 基线；0.1.0 未引入优化型 public knobs。
- [~] API review、文档、示例和 `0.1.0` 发布检查（本地门禁通过；待 crates.io 命名空间决策与远端 release CI）。
- [x] 首轮 public API 审计与 `0.1.0` 阻塞项清单。
