# Cordis Rust / Wasmtime 实现计划

> 状态：架构研究稿（2026-09-02）  
> TypeScript 对照基线：`cordis@4.0.0-rc.9`，仓库提交 `00278924a984fedfaffb4bc3d5eb7d8e76215643`（2026-09-01）  
> 论文：`2608.25512v1.txt`，*A Programming Paradigm for Spatiotemporal Composability*  
> 目标：实现 Rust 原生的 Cordis 内核，以 Wasmtime Component Model 作为唯一的运行时动态插件机制，并尽可能覆盖 TypeScript 版本的核心、Loader、Include、HMR、Timer、Logger 与宏开发体验。

## 1. 结论先行

本项目不应把 Cordis 简化成普通的依赖注入容器。Cordis 的核心是同一个 `Context` 同时承担两件事：

1. **可逆 effect**：组件对共享环境的每次改变都必须登记逆操作，卸载时精确撤销；
2. **响应式 coeffect**：组件声明依赖，依赖满足时自动激活，提供者改变或消失时自动停用并在新提供者稳定后重新激活。

Rust 版本采用以下总方向：

- 原生 Rust 组件静态链接，动态组件只使用 **WebAssembly Component Model + WIT + Wasmtime**；第一版不引入 `dlopen`/`libloading` 路线。
- 一个统一的 Fiber 生命周期同时驱动原生组件与 WASM 组件，不能为 WASM 另做一套简化生命周期。
- 用 Rust trait、泛型 key 和 procedural macro 取代 TypeScript 的声明合并、decorator 与 Proxy；运行时仍保留字符串 key/ABI hash，以支撑 WASM 动态发现。
- 使用 **单写者 Supervisor（actor）** 串行修改组件图、服务表和 Fiber 状态；组件代码与事件回调在 Supervisor 外执行，避免跨 `await` 持锁。
- WASM 使用固定的 Cordis Kernel WIT ABI。任意业务服务通过宏生成的 typed facade + 版本化消息协议穿过动态边界，而不是要求宿主在编译时知道所有第三方 WIT world。
- 所有 WASM host import 都由当前 Fiber 自动归属 effect；即使 guest 忘记释放 handle，卸载 Fiber 或丢弃 Wasmtime `Store` 时仍会清理。
- Loader 以声明式 Entry Tree 为事实来源；HMR 对 `.wasm` 采用“预编译/校验 -> 停旧 -> 启新 -> 失败回滚”的事务流程。
- 以论文中的强生命周期不变式为准：提供者进入 `Unloading` 后，先令依赖者离开并等待其清理完成，再执行提供者逆操作。这里比当前 TS 实现更明确、更安全。

## 2. 学习结果：Cordis 到底解决什么

### 2.1 论文中的最小模型

论文把动态组合拆为两个正交维度：

- **时间可组合性**：组件删除后，其贡献从系统状态中精确消失；
- **空间可组合性**：组件通过声明依赖决定何时存在于系统中，而不是依赖手写加载顺序。

Effect 的工程形式是“执行动作并返回 disposer”。多个 disposer 按 LIFO 组合；effect iterator 每完成一步就产生一个 disposer，因此异步加载中途失效时，只回滚已经完成的步骤。

Coeffect 的工程形式是 `(key -> realm -> provider/value)` 两级寻址：

- key 表示抽象能力；
- realm 控制相同 key 在不同上下文中解析到哪个隔离域；
- provider 的 Fiber 身份而非 value 相等性决定消费者是否需要 reload；
- intercept 改变“怎样使用某能力”，但不改变“解析到哪一个能力”。

组件实例是 Fiber。Fiber 将依赖声明、已提交依赖视图、effects、父上下文、配置、错误和异步迁移句柄收拢为一个生命周期边界。

### 2.2 TypeScript 实现的真实行为

当前仓库不是只有论文伪代码，而是以下包组成的工作区：

- `cordis`：Context、Fiber、Registry、Reflect、Service、Events、Logger；
- `@cordisjs/plugin-loader`：Entry Tree、Group、运行时 reconcile、realm 搬迁；
- `@cordisjs/plugin-include`：JSON/YAML、patch、原子写回；
- `@cordisjs/plugin-hmr`：模块依赖分类、缓存失效、事务回滚；
- `@cordisjs/plugin-timer`：timeout、interval、throttle、debounce；
- `@cordisjs/plugin-logger-console` 与 `@cordisjs/utils`。

核心实现确认了这些关键行为：

- `ctx.effect()` 接受同步 disposer、异步 disposer、同步 iterator 或异步 iterator；手动和自动 dispose 都是幂等的；
- effect 内部 disposer 逆序串行，Fiber 顶层 effects 可并行开始清理；
- Fiber 状态为 `Pending / Loading / Active / Failed / Unloading / Disposed`；迁移具有 inertia，已经开始的 loading/unloading 不会被强行取消，而是在边界发现 epoch 变化后串接下一次迁移；
- 失败 Fiber 不因普通依赖抖动自动复活，只有显式 `update/restart` 清除失败；
- service 只在 provider Fiber 为 `Active` 时可满足 inject；
- 消费者保存 committed view，使其在 teardown 期间仍能访问触发自己离开的旧依赖；
- provider 用单调递增、不复用的 Fiber id 标识，值相同但 provider 更换仍会触发 reload；
- 事件具有 `emit / parallel / serial / bail / waterfall` 五种模式；listener 是 effect，卸载自动注销；waterfall 的 `next` 只能调用一次；
- Loader 对 Entry 进行 keyed diff；`disabled`、`config`、`intercept`、`isolate`、父组变化采用不同的最小变更路径；
- HMR 先构建 accepted/declined 集合，再定位 stale entry，清理 ESM/CJS 缓存后重载；失败时恢复缓存和旧 Fiber。

### 2.3 论文对 Rust / WASM 的直接启示

论文第 6.4 节明确指出：

- 原生代码需要动态链接/卸载；WASM 在 Wasmtime 中可通过丢弃实例释放；
- Rust trait/impl 可承担类型层的依赖表达；
- Rust procedural macro 可同时生成 typed declaration 与访问中介，替代 decorator/Proxy；
- WASM guest 的 imports 天然形成其可达能力边界。

因此 Wasmtime 不是附加功能，而是最契合论文边界模型的动态组件实现。

## 3. 范围与兼容性原则

### 3.1 第一阶段必须实现

- Context 派生、Fiber 层级与根 Fiber；
- effect、effect iterator、嵌套 effect 元数据、幂等 dispose；
- service provide/get、inject、committed view、provider 变更通知；
- isolate realm 与 intercept；
- Fiber inertial lifecycle、失败恢复、配置 update/restart；
- 原生 Rust 组件与 Wasmtime 动态组件统一挂载；
- 五类事件的原生实现；
- WASM service/event 动态桥接；
- Loader Entry Tree、Group、Include、JSON/YAML、patch、写回；
- WASM HMR 及失败回滚；
- Logger、Timer；
- procedural macros、guest SDK、CLI/xtask；
- 对照 TS 测试的行为测试与跨 WASM 集成测试。

### 3.2 有意 Rust 化的部分

| TypeScript 能力 | Rust 方案 |
|---|---|
| `ctx.foo` Proxy 属性访问 | `ctx.get::<Foo>()` 或宏生成的 `deps.foo()` typed accessor |
| module augmentation | `ServiceKey` / `EventSpec` trait + 宏生成唯一 key 与 wire fingerprint |
| class / method decorator `@Inject` | `#[cordis::component]`、`#[cordis::inject(...)]`、`#[cordis::service]` |
| callable service | 显式 `.call(...)`，宏可生成同名 client method，不伪造 `Fn` |
| `mixin()` 把服务方法提升到 Context | extension trait / 宏生成的 Context accessor |
| 任意 JS object 作为 config | `serde_json::Value` 作为动态表示，native 端反序列化为强类型配置 |
| `!!js` | 默认使用受限 Rhai 表达式 `!expr`；只注入 Context 快照，不开放文件、网络或宿主对象 |
| Node 模块缓存 HMR | `.wasm` 内容哈希缓存 + Wasmtime Component 重建 |
| JS 长堆栈改写 | `tracing` span、Fiber/Entry 路径和 error context，不重写 Rust backtrace |

### 3.3 第一版明确不做

- 不加载 Rust `cdylib`，不依赖 `libloading`；这会引入 Rust ABI 不稳定、allocator/panic 边界和卸载安全问题；
- 不执行 JavaScript/TypeScript 插件；
- 不承诺二进制兼容 Cordis TS 插件；目标是语义和开发体验对齐；
- 不把未通过 Context/WIT import 的宿主全局副作用宣称为可逆；文件写入、外部 API 调用等仍需事务或补偿语义；
- 不在 v1 做 Fiber 内部状态迁移。HMR 默认重新 apply；后续可增加可选 `snapshot/restore` ABI。

## 4. 工作区结构

建议最终工作区：

```text
cordis_wasm/
├── Cargo.toml
├── rust-toolchain.toml
├── crates/
│   ├── cordis/                 # facade，重导出常用 API 与宏
│   ├── cordis-core/            # Context/Fiber/effect/coeffect/registry/events
│   ├── cordis-macros/          # procedural macros + compile-time diagnostics
│   ├── cordis-wasm/            # Wasmtime host、WIT bindings、WASM endpoint
│   ├── cordis-guest/           # Rust guest SDK、guest-side macros/runtime
│   ├── cordis-loader/          # Entry Tree、Group、reconcile
│   ├── cordis-include/         # JSON/YAML、patch、写回
│   ├── cordis-hmr/             # watcher、artifact cache、事务 reload
│   ├── cordis-timer/           # timer service
│   ├── cordis-logger/          # tracing exporter/console adapter
│   └── cordis-cli/             # run/check/inspect/build-component
├── wit/
│   ├── cordis-plugin.wit       # host/guest kernel ABI
│   └── deps/                   # 版本化 WIT dependencies
├── examples/
│   ├── native-counter/
│   ├── wasm-counter-provider/
│   ├── wasm-counter-consumer/
│   └── hot-reload/
├── tests/
│   ├── fixtures/               # 预构建或测试时构建的 components
│   ├── parity/                 # TS 行为移植
│   └── state-machine/          # 属性测试
└── xtask/                      # guest 构建、componentize、fixture 生成
```

`cordis-core` 不直接依赖 Wasmtime，确保内核可单独测试并允许其他 embedder。`cordis-wasm` 只实现 `ComponentFactory/ComponentInstance` 适配层。

## 5. 核心架构

```text
                declarative config / API
                           │
                           ▼
                  Loader / Entry Tree
                           │ commands
                           ▼
┌───────────────────────────────────────────────────────────┐
│                    Runtime Supervisor                     │
│  Fiber graph │ service/realm store │ event registry      │
│  target epoch│ committed views     │ effect ownership    │
└───────────────┬──────────────────────┬────────────────────┘
                │                      │
     native ComponentInstance      Wasm ComponentInstance
                │                      │
      generated typed access       Wasmtime Store actor
                │                      │ WIT kernel imports
                └──────── Context / capability boundary ────┘
```

### 5.1 为什么用 Supervisor actor

JavaScript 版本天然运行在单线程事件循环中；Rust 若直接把所有表放进多个 `RwLock`，依赖通知、事件回调、effect 注册和异步 teardown 很容易形成锁顺序与重入死锁。

Supervisor 只负责短小、确定的状态变更：

- 分配 Fiber/Effect/Listener/Realm id；
- 更新 registry、目标 epoch 和状态；
- 生成要执行的外部工作列表；
- 接收工作完成结果并决定下一状态。

组件 `apply`、disposer、事件 callback、WASM 调用不在 actor 内执行。禁止 actor 持有资源锁跨 `await`。

### 5.2 主要 ID 与数据结构

所有 ID 单调递增且不复用：

```rust
pub struct FiberId(u64);
pub struct EffectId(u64);
pub struct ListenerId(u64);
pub struct RealmId(u64);

pub struct ServiceId {
    pub name: Arc<str>,
    pub abi_hash: [u8; 32],
}
```

`abi_hash` 防止两个同名服务以不兼容的参数/返回类型互相满足。native service 宏从规范化 trait schema 生成 hash；WASM manifest 与 guest descriptor 携带同一 hash。

核心记录：

```rust
struct FiberRecord {
    id: FiberId,
    parent: FiberId,
    state: FiberState,
    desired: DesiredEpoch,
    committed: Option<CommittedView>,
    inject: Vec<InjectSpec>,
    provides: BTreeSet<(ServiceId, RealmId)>,
    effects: Vec<EffectId>,
    config: Arc<serde_json::Value>,
    transition_generation: u64,
    failure: Option<Arc<CordisError>>,
}

struct ServiceBinding {
    service: ServiceId,
    realm: RealmId,
    provider: FiberId,
    endpoint: ServiceEndpoint, // Native 或 Wasm
    availability: AvailabilityCheck,
}
```

`DesiredEpoch` 是全部 inject 所解析 provider id 的有序摘要；任一 provider 更换都会改变 epoch。

## 6. Context、Service 与 Coeffect

### 6.1 Context 是便宜的不可变视图

`Context` 本身是 `Clone` 的句柄：

```rust
pub struct Context {
    runtime: RuntimeHandle,
    node: Arc<ContextNode>,
}

struct ContextNode {
    parent: Option<Arc<ContextNode>>,
    fiber: FiberId,
    realms: HashMap<ServiceName, RealmId>,
    intercepts: HashMap<ServiceName, Arc<serde_json::Value>>,
    metadata: HashMap<MetadataKey, Arc<dyn Any + Send + Sync>>,
}
```

`extend()` 新建 overlay，不复制整棵表。`isolate()` 和 `intercept()` 都是派生 Context；丢弃派生 Context 就恢复映射，不需要显式 inverse。

### 6.2 typed 与 dynamic 两条访问路径

Native API：

```rust
pub trait ServiceKey: 'static {
    const NAME: &'static str;
    const ABI_HASH: [u8; 32];
    type Client: Clone + Send + Sync + 'static;
    type Intercept: Serialize + DeserializeOwned + Default + Send + Sync;
}

let db = ctx.get::<Database>().await?;
db.query("select 1").await?;
```

Dynamic/WASM API：

```rust
ctx.call_dynamic("app.database", abi_hash, "query", payload).await
```

两条路径最终都解析同一 committed view。组件不能通过 `get_unchecked` 绕过 inject；只有 root/system Context 有显式的 `SystemContext` 管理接口。

### 6.3 provide、可用性和唯一性

`ctx.provide::<K>(endpoint)` 本身是 effect：

1. 检查当前 Fiber active/initializing 状态；
2. 解析 Context 中 K 的 RealmId；
3. 拒绝相同 `(ServiceId, RealmId)` 的第二个 provider；
4. 登记 binding 与 provider Fiber；
5. 通知所有 inject 同一 key+realm 的 Fiber；
6. disposer 先使 binding 不再满足依赖，等待依赖者退出，再删除 binding。

Service 可定义纯同步 `availability(ctx, intercept) -> bool`。它对应 TS `Service.check`，例如 Loader 的 `await` intercept 可在 Loader 尚有任务时暂不满足消费者。

### 6.4 committed view 与访问规则

Fiber 开始 loading 时原子提交当前依赖解析结果。后续访问只读取 committed view，不直接读取实时 store：

- Active/Loading/Unloading Fiber 看到它本次加载时承诺的 provider；
- 未满足 inject 的 Pending Fiber 访问时报 `InactiveDependency`；
- 未声明依赖时报 `UndeclaredDependency`；
- provider 已进入 Unloading 后，新的消费者不能再提交它，但旧消费者在自己的 disposer 内仍能访问它。

### 6.5 isolation 与 interception

- `ctx.isolate::<K>()`：生成私有 RealmId；
- `ctx.isolate_in::<K>(RealmLabel)`：同 label 共享 RealmId；
- Loader 的 local realm 绑定 EntryId，移动 Entry 时仍随 Entry；
- global realm 绑定用户 label，最后一个引用消失时 GC；
- intercept 对 native service 使用 `K::Intercept`，对 dynamic service 使用 JSON/MessagePack value；
- intercept merge 由 service key 的 `MergeIntercept` 实现，内层覆盖外层；更新 intercept 不改变依赖满足状态。

## 7. Effect 系统

### 7.1 基础类型

避免把 `anyhow::Error` 暴露为公共 API；公共错误统一为 `CordisError`。

```rust
type BoxFutureResult = Pin<Box<dyn Future<Output = Result<(), CordisError>> + Send>>;
type Disposer = Box<dyn FnOnce() -> BoxFutureResult + Send + 'static>;

pub struct EffectGuard {
    id: EffectId,
    runtime: RuntimeHandle,
}
```

`EffectGuard::dispose()` 幂等。Drop 不能执行 async cleanup，因此 Drop 只向 Supervisor 发出 best-effort dispose 命令；正确代码必须显式 await Fiber/Runtime shutdown。Debug 构建对未完成的 async disposer 记录 warning。

### 7.2 三层 API

1. `ctx.effect(label, action)`：单动作返回单 disposer；
2. `ctx.effect_stream(label, stream)`：每一步产生 disposer，边界检查 generation；
3. `EffectScope::defer(disposer)`：宏和 service helper 使用的低层接口。

示例：

```rust
let handle = ctx.effect("listener", async move {
    let id = bus.register(handler).await?;
    Ok(disposer(async move { bus.remove(id).await }))
}).await?;
```

宏将常见注册操作包装成更短的 typed helper；用户通常不需要手写 `Pin<Box<...>>`。

### 7.3 必须保持的不变式

- disposer 至多执行一次；
- 一个 effect 内的 inverses 严格 LIFO 串行；
- iterator 失效只在 iteration boundary 中断，不取消正在执行的 step；
- step 抛错时立即逆序清理已登记 inverses；
- Fiber 卸载即使一个 disposer 失败也继续其他清理，最终聚合错误并记录；
- 子 Fiber 的 retire 是父 Fiber 的 effect，父卸载自动级联；
- inactive Context 不能新建 effect、listener、provider 或 child Fiber；
- `get_effects()` 输出 label/children 树，支撑诊断和与 TS snapshot 对照。

## 8. Fiber 生命周期

### 8.1 状态

```rust
pub enum FiberState {
    Pending,
    Loading,
    Active,
    Failed,
    Unloading,
    Disposed,
}
```

状态迁移：

```text
Pending ──deps ready──> Loading ──same epoch──> Active
   ▲                       │                       │
   │                       ├─apply error──> Failed│ deps lost/changed
   │                       │                       ▼
   └────deps absent──── Unloading <───────────────┘
                          │
                          ├─desired ready──> Loading
                          └─retired────────> Disposed
```

### 8.2 refresh / load / unload

Supervisor 在服务变更、realm 变更、config update 或显式 restart 时调用 refresh：

1. 对每个 InjectSpec 解析 `(ServiceId, RealmId, provider FiberId)`；
2. 任一缺失则 desired=`Inactive`，否则形成 epoch；
3. desired 未改变则 no-op；
4. 若当前无迁移，启动 load 或 unload；有迁移则只更新 desired；
5. load 完成时若 epoch 已变，立刻转 unload；
6. unload 完成时若又满足依赖，立刻转 load。

Rust Future 是惰性的，所以迁移必须由 Supervisor `tokio::spawn`；不能只创建 Future 后遗忘。这一点是论文专门指出的 Rust 差异。

### 8.3 强 teardown 顺序

Active provider 离开时：

1. 先将状态切到 `Unloading`，使其不再是可用 provider；
2. 通知直接消费者 refresh；
3. 等所有受影响消费者到达非活动稳定态；这会递归排空依赖图；
4. 再执行 provider 的全部 inverses；
5. 清除 committed view；
6. 根据最新 desired 进入 Pending、Disposed 或重新 Loading。

依赖图循环不会死锁等待：加载时就根据静态 InjectSpec 诊断 SCC；循环中的 Fiber 保持 Pending，并输出包含 Entry/Fiber 路径的 `DependencyCycle`。

### 8.4 失败策略

- apply/guest trap/config error -> `Failed`，已完成 effects 回滚；
- 普通依赖通知不自动重试 Failed；
- `fiber.update(config)`、`fiber.restart()`、HMR 新 artifact 可以显式恢复；
- teardown error 不阻止 Fiber 到达稳定态，但保存在 transition report 中；
- `fiber.await_idle()` 等待 inertia 链完全结束；`await_active()` 在 Failed/Disposed 时返回错误。

## 9. 组件与 procedural macros

### 9.1 原生组件 trait

```rust
pub trait Component: Send + 'static {
    type Config: DeserializeOwned + JsonSchema + Send + Sync;
    type Deps: DependencySet;

    fn descriptor() -> &'static ComponentDescriptor;

    fn apply(
        self,
        ctx: ComponentContext<Self::Deps>,
        config: Self::Config,
    ) -> impl Future<Output = Result<ComponentEffects, CordisError>> + Send;
}
```

`Component::apply(self, ...)` 是生成给运行时的 owning adapter：它把实例移入异步串行的
`ComponentCell`。用户标注 `#[cordis::apply]` 的方法使用 `&mut self`，从而让 apply 与所有
method-level inject 在同一个组件实例上执行；生成的 adapter 不会在 apply 后丢弃实例。

运行时将原生和 WASM 都抹平成：

```rust
trait ComponentFactory: Send + Sync {
    fn descriptor(&self) -> &ComponentDescriptor;
    async fn instantiate(&self, host: InstanceHost) -> Result<Box<dyn ComponentInstance>>;
}

trait ComponentInstance: Send {
    async fn activate(&mut self, ctx: Context, config: Value) -> Result<()>;
    async fn deactivate(&mut self) -> Result<()>;
    async fn call_service(&mut self, call: DynamicCall) -> Result<Payload>;
    async fn call_event(&mut self, call: EventCall) -> Result<EventReply>;
}
```

### 9.2 宏设计

```rust
#[cordis::service(name = "app.counter")]
pub trait Counter {
    async fn add(&self, delta: u64) -> Result<u64, CounterError>;
    async fn get(&self) -> Result<u64, CounterError>;
}

#[cordis::component(name = "counter-consumer")]
#[cordis::inject(Counter, Logger)]
pub struct Consumer;

#[cordis::component_impl]
impl Consumer {
    #[cordis::apply]
    async fn start(
        &mut self,
        ctx: ComponentContext<ConsumerDependencies>,
        cfg: Config,
    ) -> Result<()> {
        ctx.deps().counter.add(cfg.initial).await?;
        Ok(())
    }

    #[cordis::inject(Timer)]
    async fn bind_timer(
        &mut self,
        ctx: MethodContext<ConsumerBindTimerDependencies>,
    ) -> Result<()> {
        // 生成一个依赖 Timer 的 child Fiber，等价于 TS method @Inject。
        Ok(())
    }
}
```

宏负责生成：

- `ServiceKey`、typed client、native dispatcher、wire codec 与 ABI hash；
- ComponentDescriptor、InjectSpec、ProvideSpec、配置 schema；
- 访问器只暴露声明过的 deps，尽量把 undeclared access 变成编译错误；
- method-level inject 对应一个自动登记的 child Fiber；
- guest 侧 service/event dispatch match；
- 清晰的 `syn::Error`，并用 `trybuild` 固化 compile-fail 文案。

宏不依赖 nightly，不解析跨 crate Rust 源文件；所有跨 crate 信息通过 trait/const/生成 schema 传递。

method-level inject 的 native 路径已经落地为：

- 每个注入方法生成独立的 `XxxMethodDependencies`，因此父 Fiber 不会被迫 inject 仅供方法使用的服务；
- `MethodFiberRuntime` 由 `RuntimeHandle` 和 `NativeServiceRegistry` 组成，`ComponentContext::with_method_runtime`
  将它交给 owning adapter；没有该 runtime 时明确返回 `MissingMethodRuntime`；
- child Fiber 继承父 Context 的 realm/intercept overlay，但拥有独立 FiberId、committed view 和 EffectSet；
- Supervisor 仍只计算状态迁移；注册在 `RuntimeHandle` 外侧的 executor 执行用户方法和异步 disposer，
  不会在 Supervisor actor 内运行用户代码；
- 同一组件实例的 apply 和多个注入方法通过 `ComponentCell` 串行取得 `&mut self`，避免并发可变访问；
- provider 出现时加载 child，丢失或替换时先 dispose 方法 EffectSet 再重载；父 EffectSet 的 disposer
  retire child 并等待其到达 `Disposed`，实现父卸载级联；
- `create_live_child_fiber` 只接受处于 `Loading` 或 `Active` 的父 Fiber，已失活的 Context 不能登记 child。

当前 native service API 已落地为两条语义一致但成本不同的路径：

- `FooClient::from_native(Arc<T>)` 使用宏生成的 object-safe typed adapter，native-to-native 调用不做序列化；
- `FooClient::new(Arc<dyn ServiceDispatcher>)` 校验完整 `ServiceId` 后，通过 MessagePack payload 调动态 dispatcher，供后续 Wasmtime 路由直接复用；
- `FooDispatcher<T>` 将 native provider 暴露成动态 dispatcher，测试和 WASM host 不需要手写 method match；
- client 始终返回 `Result<T, ServiceCallError<E>>`，明确区分业务错误 `E` 与 codec、ABI、路由等 transport 错误；
- service 方法只接受 owned 参数并返回 `Result<T, E>`。宏把声明的 `async fn` 改写为带 `Send` 约束的 RPITIT，避免不保证 `Send` 的 async trait future 进入 Fiber；
- method id 与 service ABI hash 只由 service 名、方法名、参数类型顺序和返回类型构成，方法声明顺序、注释、可见性和参数改名不会改变线协议；
- `#[cordis::inject(Foo)]` 生成 `FooClient` 类型的依赖字段和唯一构造函数，组件不能从依赖集合访问未声明服务。

当前 MessagePack 使用位置参数 tuple，字段名不参与 ABI；这既保持编码最小，也与 owned 参数和规范化 method id 一致。native fast path 与 dynamic path 已由同一集成测试验证，`native_counter` 示例执行真实注入调用。

### 9.3 definition-site / use-site 语义

TS 通过 traceable Proxy 区分服务定义位置和调用位置。Rust 版本由宏生成的 client 显式携带 `CallContext`：

- provider 内部声明的依赖按 provider definition context 解析；
- service method 产生的新 effect、intercept 与 isolate 按 consumer use context 归属；
- client 自动附加 caller FiberId，用户不能伪造；
- 使用 `cordis::spawn` 时显式传播 CallContext；裸 `tokio::spawn` 不传播，并在需要 Context 时返回 `MissingCallContext`。

这比依赖 task-local 魔法更可检查；task-local 只用于 tracing，不作为权限依据。

## 10. 事件系统

### 10.1 typed event

```rust
#[cordis::event(name = "agent/before-run", mode = "waterfall")]
pub trait BeforeRun {
    type Input = RunRequest;
    type Output = RunRequest;
}
```

宏生成 EventSpec、codec、ABI hash 和 typed runtime dispatch helper。后续动态运行时注册表以 `(EventName, AbiHash)` 为 key，以单调 ListenerId 保持注册顺序。

当前宏层已生成 `EventId`、`EventMode`、`FooRuntime`、输入/输出 MessagePack codec 和按 mode 唯一确定的 `FooEvent::dispatch`：

- `emit` 委托给 `AsyncEvent::emit_nowait`，显式接收异步错误 sink；
- `parallel` 返回保持 listener 顺序的全部 `ControlFlow`；
- `serial` 与 `bail` 返回首个 break value，其中 bail 保持同步；
- `waterfall` 委托给 one-shot `Next` onion runtime，并在宏展开时要求 `Input` 与 `Output` 类型完全一致；
- 未指定 mode 时明确默认为 `parallel`，未知 mode、重复或未知 attribute key 都是编译错误；
- event ABI hash 由 event 名、mode、输入类型和输出类型构成，不包含 trait 名、注释或可见性。

native 调用直接使用强类型 payload；MessagePack codec 只为后续 WASM callback/envelope 边界准备，不给 native 路径增加序列化成本。

### 10.2 五种模式

- `emit_nowait`：按注册顺序启动 callback，不等待；同步启动错误立即返回，异步错误交给 error sink；
- `parallel`：并行等待全部 listener，返回 `AggregateError`；
- `serial`：按序 await，listener 返回 `ControlFlow::Break(value)` 时停止；
- `bail`：仅允许同步 listener，首个 `Break` 停止；
- `waterfall`：onion middleware；`Next` 是不可 Clone 的 one-shot 值，类型层阻止常见的重复调用，运行时仍保留二次调用检查。

事件 filter 以 Context realm 匹配实现；`global` listener 跳过 filter；`prepend` 使用独立优先级段而不是负数 id。

### 10.3 WASM waterfall 风险门

普通 emit/parallel/serial/bail 可以用 guest callback id + payload 直接桥接。waterfall 的 guest 在等待 `next()` 时可能要求宿主调用另一个 listener；如果后续 listener 位于同一个 Wasmtime Store，会产生同实例重入问题。

实施前必须做独立 spike 验证 Wasmtime 48 的 `component-model-async` / concurrent call 能否安全支持：

1. guest A listener 调 host `next`；
2. host 调 guest B；
3. B 返回后恢复 A；
4. 后续链再次命中 A 同一 Store；
5. 检查取消、trap 和 one-shot token 回收。

若同 Store 重入不可用，v1 的 WASM waterfall 采用显式限制：同一 Fiber 对同一 waterfall event 至多一个 listener；违反时加载失败，而不是悄悄改变 onion 语义。native waterfall 保持完整兼容。后续再实现 CPS/two-phase continuation ABI。此项是 Phase 0 的 go/no-go gate。

Wasmtime 48.0.1 spike 的实际结果为带约束的 **go**：async host import 挂起期间，host 可用同一 Store 递归调用同一 Component instance 的同一 `dispatch` export，三层 onion 返回与外层恢复均正确。v0.1 因而保留完整 waterfall `next()` 语义，host 负责 one-shot token、过期检查和 trap/teardown 回收。当前行为不要求 `component-model-async` feature，独立顶层并发调用仍保持关闭。

取消有严格限制：直接 drop 一个停在 async host import 中的 `call_async` future，会让后续调用得到 `CannotEnterComponent`。实例 actor 禁止以 `tokio::time::timeout` 丢弃 in-flight Wasmtime future；guest 计算由 fuel/epoch 内部 trap，host import 必须合作取消并正常返回。不可合作取消时销毁整个 Store，不再复用 instance。可执行证据与限制记录在 `docs/wasmtime-findings.md`。

## 11. Wasmtime 动态插件方案

### 11.1 版本与功能

以 **Wasmtime 48.0.x LTS** 为基线，初始锁定已发布补丁版 `48.0.1`；启用最小 feature 集：

- `runtime`
- `cranelift`（开发/JIT；生产可选预编译）
- `component-model`
- `component-model-async`（经 spike 后决定是否必选）
- `async`
- `cache`

WASI 使用 preview 2 / WASIp2。默认不继承宿主环境、目录、网络、stdio；能力来自 manifest + intercept 后的最小授权。

### 11.2 为什么不是“每个服务一个静态 WIT import”

第三方 Cordis 插件能在宿主发布后引入新 service。若宿主只用 `wasmtime::component::bindgen!` 静态生成所有业务接口，它必须提前知道所有服务，失去 Cordis 的动态扩展能力。

因此采用两层 ABI：

1. **Kernel WIT** 固定、强类型、版本化，只描述组件生命周期、注册、调用、事件、日志和错误；
2. **业务服务协议**由 `#[cordis::service]` 生成，参数/返回值用确定性 MessagePack payload，附 service ABI hash 和 method id。

native-to-native 直接走 typed trait，不序列化；任何跨 WASM 边界的调用走动态 envelope。未来可为宿主已知的高频服务增加专用 WIT fast path，但不改变语义。

### 11.3 Kernel WIT 草案

```wit
package cordis:plugin@0.1.0;

interface types {
  record dependency { key: string, abi-hash: list<u8>, intercept: option<list<u8>> }
  record provision { key: string, abi-hash: list<u8> }
  record descriptor {
    name: string,
    version: string,
    abi-version: string,
    inject: list<dependency>,
    provide: list<provision>,
    config-schema: list<u8>,
  }
  record call { key: string, abi-hash: list<u8>, method: u32, payload: list<u8> }
  variant call-error { unavailable(string), denied(string), invalid(string), failed(string) }
  variant event-reply { continue_(list<u8>), break_(list<u8>) }
}

interface host {
  resource registration;
  provide: func(key: string, abi-hash: list<u8>) -> result<registration, string>;
  listen: func(event: string, abi-hash: list<u8>, callback: u64, mode: u8) -> result<registration, string>;
  call: func(request: types.call) -> result<list<u8>, types.call-error>;
  dispatch: func(event: string, abi-hash: list<u8>, payload: list<u8>) -> result<list<u8>, string>;
  log: func(level: u8, target: string, message: string);
}

world plugin {
  import host;
  export descriptor: func() -> types.descriptor;
  export activate: func(fiber: u64, config: list<u8>) -> result<_, string>;
  export deactivate: func() -> result<_, string>;
  export call-service: func(request: types.call) -> result<list<u8>, types.call-error>;
  export call-event: func(callback: u64, payload: list<u8>) -> result<types.event-reply, string>;
}
```

最终语法以 Wasmtime 48/WIT parser 验证为准；草案表达的是协议职责，不是可直接提交的最终 WIT。

### 11.4 实例隔离与调用模型

每个 WASM Fiber 拥有独立：

- `Store<GuestState>`；
- Component instance；
- ResourceTable；
- 调用队列/actor；
- FiberId、Context、已授权 capability；
- fuel/epoch deadline、memory/table/resource limits；
- guest registration -> host EffectId 映射。

Wasmtime `Store` 不是并发共享对象。每实例 actor 串行普通 service call；不同实例可并行。检测 guest -> host -> 同 guest 的循环调用，默认返回 `ReentrantCall`，避免无界死锁。若 Component Model async spike 证明安全，再开放受控并发。

### 11.5 effect 归属与卸载

Guest 调 `provide/listen/timer/...` 时无须传 FiberId；host binding 从当前 Store 的 GuestState 取得真实 FiberId并登记 effect。返回的 WIT `registration` resource 支持提前释放。

Wasmtime 48.0.1 spike 已验证：guest 显式执行 `resource.drop` 会调用 host destructor，但 guest 若丢失 own handle，随后 drop Store **不会**补调用该 destructor。因此 ResourceTable 只用于句柄互操作，不能作为强制清理机制；host 创建 registration 的同一事务必须把对应 disposer 登记到 Fiber `EffectGuard`，resource destructor 只触发该 effect 的幂等提前释放。

卸载顺序：

1. Fiber 不再对新消费者可见；
2. 排空依赖者；
3. 调 guest `deactivate`，给 guest 释放内部状态的机会；
4. host 强制 dispose 该 Fiber 尚存的 registrations/effects；
5. 终止 guest tasks，清空 ResourceTable；
6. drop Store/Instance；
7. 更新状态并发出诊断事件。

不能依赖 guest 善意 cleanup；host effect 表才是最终权威。

### 11.6 安全限制

`RuntimeBuilder` 必须提供：

- 每次调用 fuel 配额与补充策略；
- epoch interruption / deadline；
- max linear memory、table elements、instances、resources；
- payload 大小、嵌套深度和每 Fiber 并发调用上限；
- WASI preopen 目录的读写区分与路径规范化；
- 默认禁网；显式网络 capability 使用 host service，不直接 inherit sockets；
- manifest 签名/hash 校验 hook；
- trap、OOM、timeout 到 CordisError 的稳定映射；
- host panic 永不穿过 ABI；所有 binding catch/unwind 并转为 trap/error。

## 12. Loader、Include 与配置 reconcile

### 12.1 Entry 格式

```rust
pub struct EntryOptions {
    pub id: EntryId,
    pub component: ComponentRef, // builtin:、file:、可扩展 registry scheme
    pub config: serde_json::Value,
    pub group: bool,
    pub disabled: bool,
    pub inject: Vec<DynamicInjectOverride>,
    pub intercept: BTreeMap<String, Value>,
    pub isolate: BTreeMap<String, IsolateRule>,
}
```

`ComponentRef` 不允许隐式任意网络下载。远程 registry 是后续可插拔 resolver，并必须经过 hash/signature policy。

### 12.2 reconcile 规则

- `id` 或 component artifact 改变：重建 Fiber；
- `disabled=true`：retire；恢复 false：重新 instantiate；
- `config`：schema 校验后 `fiber.update`；组件可通过配置相等策略避免无意义 reload；
- `intercept`：Context overlay 原位更新，不触发依赖 epoch；
- `isolate`：计算 realm diff，搬迁属于该 Entry scope 的 binding，再精确 notify；
- parent/group 移动：保留 EntryId/local realm，重接 Context parent 并重算受影响依赖；
- group config：以 child id 做 keyed diff，并发准备，Supervisor 串行提交；
- self-update/self-disable 写回 Entry Tree；写回走临时文件 + fsync（可配置）+ atomic rename。

### 12.3 Include

- 支持 JSON 与 YAML；不使用已经停止维护的 `serde_yaml`，采用活跃维护的 `serde-saphyr`；
- YAML 自定义 `!expr` 保存为 AST 节点，直到 Entry 激活时才求值；
- `initial` 在文件不存在时创建；
- patch 支持按 id override、name 一致性检查、root/group insert；
- 只读文件拒绝写回但仍可加载；
- 保留原始文档顺序；第一版不承诺保留注释，后续可用 parser 的 comment capture 能力增强；
- Rhai Engine 禁止 eval/import、文件、网络、时间和无限循环，设置最大表达式深度/operations；只暴露序列化后的服务只读快照。

## 13. WASM HMR

### 13.1 与 Node HMR 的差异

WASM component 通常已经把 Rust guest 依赖编进单个 artifact，不能照搬 Node 的 ESM import cache 图。v1 以这些文件为 stale source：

- `.wasm` artifact；
- sidecar/embedded manifest；
- Entry config include；
- manifest 显式声明的外部资源。

### 13.2 reload 事务

1. watcher debounce 并用内容 hash 去重；
2. 读取完整文件到内存，避免编译半写入 artifact；
3. `Component::from_binary` 编译候选；
4. 校验 WIT world、Kernel ABI major、descriptor、schema、capability 与资源限额；
5. 建立 candidate instance，但不调用 `activate`；
6. 记录旧 artifact bytes/compiled Component/config/Entry 关联；
7. retire 旧 Fiber并等待稳定；
8. 用 candidate 启动新 Fiber；
9. 成功后提交缓存和 Entry 关联并发 `hmr/reload`；
10. 任一步失败：drop candidate，重新实例化缓存中的旧 Component；旧实例已不能安全复用，因此是“从旧 artifact 重建”，不是复活旧 Store；
11. 若旧 artifact 也无法恢复，Entry 进入 Failed，其他 Entry 保持不变并输出明确 rollback report。

多个 stale Entry 先全部 prepare，再按稳定 EntryId 顺序 commit；失败则回滚本批已触及项。跨服务的绝对无缝原子切换需要 shadow realm/双写，v1 只保证最终一致和不留下半注册 effects，不承诺调用零间隙。

### 13.3 编译缓存

- cache key：`blake3(wasm bytes + wasmtime version + engine config + target)`；
- 进程内缓存 compiled `Component`；
- 磁盘 AOT cache 仅通过 Wasmtime 官方 cache config；
- 绝不对不可信 bytes 直接调用 unsafe deserialize；
- 缓存有容量/TTL并暴露命中率。

## 14. Logger、Timer 与辅助能力

### 14.1 Logger

- 内核统一用 `tracing` event/span；
- Fiber、Entry、component、realm、call id 进入 span fields；
- logger service 支持多个 exporter、按 target level、固定容量环形 buffer；
- exporter 注册是 effect；
- WASM `host.log` 自动补 Fiber 元数据并限制单条长度；
- console exporter 独立 crate，核心不依赖终端颜色库。

### 14.2 Timer

- `timeout(callback, delay)` 返回 EffectGuard；
- `sleep(delay)` 在 Context dispose 时返回 `ContextDisposed`；
- `interval` 同时支持 callback 与 `Stream`；
- `throttle/debounce` 返回可 dispose wrapper；
- 所有 Tokio task 都挂到 Fiber task group，Fiber unload 会 cancel 并 await；
- 测试用 `tokio::time::pause/advance`，不依赖真实时钟。

### 14.3 List/Registry helper

提供 effect-aware `TrackedList<T>`、`TrackedMap<K,V>`。每次插入分配唯一 registration id，disposer 按 id 删除，避免相同值注册互相误删；这也符合论文对可交换注册表的要求。

## 15. 配置 schema 与 manifest

Native config：

- `serde` 反序列化；
- `schemars` 使用显式 `SchemaSettings::draft2020_12()` 生成 schema，不依赖库的可变默认值；
- 当前 native descriptor 已生成 schema，并用 trait bound 在编译期约束 `DeserializeOwned + JsonSchema`；
- `jsonschema` 校验、可选的跨字段校验 hook，以及带 JSON Pointer path 的配置错误，在动态组件入口落地时一并实现；当前没有动态配置入口，避免预先加入无调用点 API。

WASM component descriptor 至少包括：

```toml
name = "example.counter"
version = "0.1.0"
kernel-abi = "0.1"
artifact = "counter.wasm"
sha256 = "..."

[[inject]]
key = "cordis.logger"
abi = "..."

[[provide]]
key = "example.counter"
abi = "..."

[capabilities]
clock = true
random = false
network = false
read = ["./data"]
write = []
```

构建工具把 descriptor/schema 同时嵌入 component custom section，并可生成 sidecar。加载时两者不一致即拒绝，防止替换 artifact 后沿用旧权限声明。

## 16. 依赖策略（禁止旧依赖）

### 16.1 工具链基线

- Rust edition 2024；
- 初始 MSRV 与 CI stable 固定为 **Rust 1.98.0**；当前机器是 1.95.0，进入实现阶段前需升级；
- Wasmtime 固定在 **48.0.x LTS**，起始 `48.0.1`，只跟随该 LTS 的 patch；
- 不使用 Wasmtime `49.0.0-dev`、notify `9.0.0-rc` 等预发布依赖。

### 16.2 起始依赖集合

版本号在创建 `Cargo.lock` 当天用 `cargo add` 再确认；本研究日已核对的基线包括：

- `wasmtime 48.0.1`；
- `tokio 1.53.1`；
- `serde 1.0.229`；
- `schemars 1.2.2`；
- `jsonschema 0.52.0`；
- `notify 8.2.0`（最新稳定版）；
- `rhai 1.26.0`；
- `serde-saphyr 1.2.0`。

其余优先采用当前稳定版：`thiserror`、`tracing`、`tracing-subscriber`、`futures`、`bytes`、`rmp-serde`、`blake3`、`semver`、`toml`、`url`、`indexmap`、`syn 2`、`quote 1`、`proc-macro2 1`。

原则：

- `default-features = false`，只打开需要的 feature；
- runtime crate 不引入 CLI/测试依赖；
- `cargo-deny` 检查 advisory、重复大版本、license 和来源；
- 每周 Dependabot/Renovate PR，Wasmtime 只在同 LTS patch 自动合并；
- `cargo update -w --locked`、`cargo audit`、`cargo tree -d` 进入 CI；
- 禁止 git dependency 和未固定 revision；
- 禁止已弃用/无人维护的 `serde_yaml`；
- 发布前记录 MSRV，并运行 `cargo +1.98.0 check --workspace --all-targets`。

## 17. 测试计划

### 17.1 直接移植 TS 行为

以当前仓库约 4,689 行测试为 checklist，不逐字翻译，但覆盖每一个行为组：

- effect：手动/自动 dispose、sync/async/stream、边界 abort、错误回滚、effect tree；
- events：symbol 等价 key、once、filter、五种 dispatch、aggregate error、waterfall one-shot/nested async；
- Fiber：三类 inertia 抖动、failed 不自动重入、update 恢复、dispose error；
- plugin：function/object/class 对应的 Rust factory、嵌套 Fiber、root shutdown、重复 dispose；
- service：pending inject、provider init 未完成不可见、多 inject、definition/use context；
- reflect/access：未声明、inactive、重复 provide、泄漏防护；
- isolate：private/shared realm、provider/consumer 两侧变更、事件过滤；
- loader：group enable/disable/transfer/intercept、self-update/self-dispose；
- managed isolate：provider/consumer realm diff、nested realm、跨 group transfer；
- include：无 patch、override、name mismatch、missing id、root/group insert、多 patch；
- HMR：单/多组件、依赖更新、syntax/compile error rollback、批处理、service/event 清理、rapid reload；
- timer：timeout/sleep/interval stream/concurrent reads/return/throw/dispose/throttle/debounce。

### 17.2 Rust 专属测试

- `trybuild`：未声明 dependency、错误 service signature、非 Send future、重复 service key、非法宏组合；
- `proptest` 状态机：随机 provide/withdraw/isolate/move/update/dispose 后与 reference model 对比；
- 可逆性属性：任意成功 effect 序列全清理后 snapshot 与初始相同；
- 收敛性属性：同一最终 Entry set 的多种操作顺序得到相同 quiescent active set；
- 依赖排空顺序：consumer disposer 完成前 provider disposer 不开始；
- 并发：大量 notify/config/HMR 命令下无丢更新、无重复 disposer；
- WASM：trap、fuel exhaustion、epoch timeout、OOM、超大 payload、guest 遗漏 handle、guest panic；
- capability：未授权 fs/network/env 访问失败；路径穿越和 symlink escape；
- ABI：hash mismatch、kernel major mismatch、旧 minor 兼容；
- HMR：半写 artifact、候选 instantiate 失败、新 apply 失败、旧 artifact rollback 失败；
- sanitizer/Miri 覆盖不含 Wasmtime 的 core；必要时用 Loom 验证 EffectGuard 的 exactly-once 状态机。

### 17.3 性能基准

- 10k Fiber 依赖通知；
- 10k listener 注册/卸载与五类 dispatch；
- native service call、native->WASM、WASM->native、WASM->WASM 延迟；
- component cold compile / warm cache / instantiate；
- 1k Entry reconcile 与批量 HMR；
- effect tree 内存与 unload 延迟。

性能优化不能绕过 Context mediation；如果 fast path 不能保留 Fiber 归属和 intercept，就不接受。

## 18. 分阶段落地

### Phase 0：风险验证与行为规格

产物：`docs/semantics.md`、可执行 Wasmtime spikes、TS parity checklist。

任务：

1. 建 Cargo workspace、Rust 1.98 toolchain、CI、lint/deny/audit；
2. 固定 Cordis TS commit，并把测试名称/行为转换成追踪表；
3. 验证 Wasmtime 48 Component + WIT host/guest 最小调用；
4. 验证 async host import、fuel、epoch、ResourceTable/drop；
5. 验证 waterfall 同 Store 重入，决定限制或 concurrent ABI；
6. 验证 `wasm32-wasip2` guest build/componentize 工具链；
7. 冻结 Kernel WIT `0.1.0` 与兼容规则。

退出标准：没有未验证的 Wasmtime API 假设；最小 guest 能动态加载、调用、注册 effect、卸载且宿主状态恢复。

### Phase 1：cordis-core

1. IDs、Supervisor、Context overlay；
2. EffectGuard、effect stream、effect tree；
3. ServiceId/RealmId/store/committed view；
4. Fiber lifecycle 与依赖排空；
5. typed/dynamic events；
6. errors、diagnostics、tracing；
7. 移植 core parity tests 与 property tests。

退出标准：不接 Wasmtime，仅用 mock ComponentInstance 已通过全部核心语义和随机状态机测试。

### Phase 2：宏与 native DX

1. `service/event/component/inject` 宏；
2. typed client/server dispatcher；
3. config schema 与 ABI hash；
4. method inject child Fiber；
5. trybuild compile-pass/fail；
6. native counter 示例和 API 文档。

退出标准：常规插件代码不接触 type erasure、payload codec 或手写 disposer boxed future。

当前状态：Phase 2 已完成。config schema 固定为 Draft 2020-12；trybuild 覆盖配置 trait
约束、未声明依赖、非 Send future、重复 inject、非法宏组合和现有 service/event/apply 签名诊断。

### Phase 3：cordis-wasm / guest SDK

1. Kernel WIT 和双侧 bindings；
2. Wasmtime engine/store/linker/limits/WASIp2；
3. lifecycle adapter；
4. service/event codec 与跨边界路由；
5. registration resource 与强制 cleanup；
6. guest macros/SDK/xtask；
7. provider/consumer 双 WASM 端到端示例；
8. trap/limit/capability/ABI tests。

退出标准：删除 `.wasm` Fiber 后，service/listener/timer/task 全部消失；替换 provider 会准确重载 consumer。

### Phase 4：Loader / Include

1. Entry Tree、Group、resolve/move/update/remove；
2. managed realm 与 delimiter 等价机制；
3. JSON/YAML、patch、atomic write；
4. Rhai `!expr` 安全求值；
5. self-update/self-disable；
6. loader/include/isolate parity tests。

退出标准：相同最终配置从空载入和增量 reconcile 得到相同 active set 与 service snapshot。

### Phase 5：WASM HMR

1. notify watcher、debounce、hash；
2. candidate preflight/prepare；
3. batch transactional commit/rollback；
4. cache 与 reload report；
5. HMR parity + failure injection tests。

退出标准：连续快速更新、坏 artifact、apply trap 均不留下重复 provider/listener，也不破坏未触及 Entry。

### Phase 6：Timer、Logger、CLI 与发布质量

1. timer/logger/console；
2. `cordis run/check/inspect/build-component`；
3. graph/effect tree 诊断输出；
4. benchmarks、docs、examples；
5. cargo-deny/audit/MSRV/跨平台 CI；
6. API review 后发布 `0.1.0`。

## 19. 完成定义

`0.1.0` 只有满足以下条件才算完成：

- 核心时间/空间可组合性不变式有 property test；
- TS 核心、loader、include、HMR、timer 的行为 checklist 全部标为 pass/adapted，并为 adapted 项提供文档；
- native 和 WASM 使用同一 Fiber、service store、event registry 和 effect ownership；
- WASM guest 无法访问未声明/未授权的服务和 WASI 能力；
- guest 不合作、trap 或超时后仍能由 host 清理；
- HMR 失败能恢复旧 artifact 或给出明确不可恢复状态，不存在半注册；
- public API 无 `unsafe` 要求；内部 `unsafe` 仅限上游 Wasmtime API 必需点并有安全注释；
- `cargo test --workspace --all-features`、Clippy `-D warnings`、fmt、doc、deny、audit、MSRV 全绿；
- Linux/macOS/Windows 至少覆盖 x86_64，Linux/macOS 覆盖 aarch64；
- 依赖均为实现时的当前稳定版本或明确选择的 Wasmtime LTS patch，无废弃 crate、无预发布 runtime 依赖。

## 20. 主要风险与提前决策

| 风险 | 处理 |
|---|---|
| Wasmtime Component async/同 Store 重入不足 | Phase 0 spike；WASM waterfall 加载时显式限制，不静默降级 |
| 动态服务失去 WIT 端到端类型 | 宏生成 typed facade + ABI hash + schema；跨边界才 type erase |
| Supervisor 与 callback 相互等待 | actor 不跨 await 持状态；callback 只通过命令/响应交互；建立 reentrancy 测试 |
| async Drop 无法保证清理 | 显式 `shutdown/await_idle` 为正确路径；Drop 只 best-effort；host 统一 task group |
| 配置表达式变成逃逸口 | Rhai 最小 engine、operation limit、只读数据、无 I/O/模块/eval |
| HMR 无法真正原子切换多个 provider | v1 保证回滚和最终一致，文档声明短暂不可用窗口；后续研究 shadow realm commit |
| Rust service trait object 不支持直接 async dyn | service 宏生成 boxed dispatcher/client，不把用户 trait 直接作为 dyn 公共边界 |
| Provider/consumer dispose 环形成死锁 | 加载前 SCC 诊断；运行时等待图断言；循环保持 Pending |
| Wasmtime 大版本更新快 | 选择 48 LTS；只自动升级 patch；下一 LTS 单独兼容分支 |

## 21. 资料与版本依据

- DeepSeek Harness Cordis Primer：<https://deepseek-harness.github.io/deepseek-harness/reference/cordis-primer>
- Cordis TypeScript 实现：<https://github.com/cordiverse/cordis>，本地对照提交 `00278924a984fedfaffb4bc3d5eb7d8e76215643`
- 论文 arXiv：<https://arxiv.org/abs/2608.25512>；本项目使用根目录 `2608.25512v1.txt`
- Wasmtime 48.0.1 crate：<https://docs.rs/crate/wasmtime/48.0.1>
- Wasmtime Component bindgen：<https://docs.wasmtime.dev/api/wasmtime/component/macro.bindgen.html>
- Wasmtime WASIp2 示例：<https://docs.wasmtime.dev/examples-wasip2.html>
- Wasmtime LTS RFC：<https://github.com/bytecodealliance/rfcs/blob/main/accepted/wasmtime-lts.md>
- WebAssembly Component Model：<https://component-model.bytecodealliance.org/>
- Rust 1.98.0：<https://blog.rust-lang.org/2026/08/20/Rust-1.98.0/>
- Rust 2024 Edition：<https://doc.rust-lang.org/book/appendix-05-editions.html>
- 当前依赖版本查询：<https://docs.rs/>

## 22. 下一步

按此计划开始实现时，第一批提交应严格只做 Phase 0：workspace/CI、Kernel WIT spike、effect/resource cleanup spike、waterfall reentrancy spike 和行为追踪表。不要先大面积写业务 API；WASM 重入与资源析构验证结果会直接决定事件 ABI 和 `ComponentInstance` trait 的最终形状。
