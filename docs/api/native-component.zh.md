# 原生组件

原生编写路径（native authoring path）让你用普通 Rust 编写组件，静态链接，直接路径上零序列化。
`#[cordis::component]` / `#[cordis::component_impl]` 这一对宏生成 `Component` 实现、依赖集，以及
supervisor 用来挂载（mount）和驱动组件的 descriptor。本页记录这些宏所消费的 trait 与类型，以及你
要交互的运行时类型；宏本身的契约见 [macros](macros.zh.md)。

## `ComponentDefinition`

```rust
pub trait ComponentDefinition: Send + 'static {
    type Config: DeserializeOwned + JsonSchema + Send + Sync + 'static;
    type Deps: DependencySet;

    fn descriptor() -> &'static ComponentDescriptor;
}
```

生成组件 adapter 所共享的元数据与关联类型。`type Config` 是类型化 config
（`DeserializeOwned + JsonSchema`）；`type Deps` 是生成的依赖集；`descriptor()` 返回缓存的
descriptor。

## `ComponentDescriptor`

```rust
#[derive(Clone, Debug)]
pub struct ComponentDescriptor {
    pub name: &'static str,
    pub injects: Vec<InjectSpec>,
    pub config_schema: fn() -> Schema,
}
```

由 `#[cordis::component]` 生成的不可变元数据。`name` 是组件名；`injects` 是已声明的依赖；
`config_schema` 是一个返回 Draft 2020-12 JSON Schema 的函数。

## `Component`

```rust
pub trait Component: ComponentDefinition {
    fn apply(
        self,
        context: ComponentContext<Self::Deps>,
        config: Self::Config,
    ) -> impl Future<Output = Result<ComponentEffects, CordisError>> + Send;
}
```

可执行的组件。由 `#[cordis::component_impl]` 展开实现。`apply` 就是 effect body：给定组件的
context（依赖 client、fiber context、effect set）与其类型化 config，它注册 effect 并返回一个
`ComponentEffects`，把保留的 `EffectSet` 交还给 fiber。出错时它会 dispose 这些 effect 并返回错误。

## `ComponentContext<D>`

```rust
#[derive(Clone, Debug)]
pub struct ComponentContext<D: DependencySet> { /* context, deps: Arc<D>, effects, method_runtime */ }

impl<D: DependencySet> ComponentContext<D> {
    pub fn new(context: Context, deps: D, effects: EffectSet) -> Self;
    #[must_use] pub fn with_method_runtime(mut self, runtime: MethodFiberRuntime) -> Self;
    pub fn context(&self) -> &Context;
    pub fn deps(&self) -> &D;
    pub fn effects(&self) -> &EffectSet;
    pub fn effect_set(&self) -> EffectSet;
    pub async fn register_method<D2, F, Fut>(
        &self, label: &'static str, callback: F,
    ) -> Result<FiberId, CordisError>
    where
        D2: DependencySet,
        F: Fn(MethodContext<D2>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), CordisError>> + Send + 'static;
}
```

交给 native 组件 `apply` 方法的 context。`deps()` 给出类型化的依赖 client（例如
`context.deps().counter`）；`effect_set()` 给出 fiber 的 `EffectSet`，用于在其下注册 effect；
`context()` 给出用于 realm/intercept 解析的 `Context`。

### `with_method_runtime`

为本次组件加载启用方法级 inject 注册。当 `MethodFiberRuntime` 可用时，由挂载该组件的代码调用；
没有它时，`register_method` 返回 `CordisError::MissingMethodRuntime`。

### `register_method`

把一个生成的方法注册为 **effect 拥有的 child fiber**。该方法成为一个独立的 fiber，其 context
扩展自本组件的 context，并拥有依据 committed view 解析出的自己的 `DependencySet`；它通过所提供的
`MethodFiberRuntime` 作为 executor 安装到 supervisor 上。该方法的 effect set 会在 unload 时被
dispose，而 retire 该 child fiber 的动作会作为 disposer 推迟进本组件的 effect set——因此当组件
unload 时，它的 method fiber 会被拆除。

这正是 `#[cordis::component_impl]` 中方法上的 `#[cordis::inject]` 的工作原理：每个这样的方法都是
一个拥有自身依赖的 child fiber。

**错误** —— 如果该 context 没有 method runtime、父级处于非活动状态、依赖配置失败，或所属 effect
已开始 dispose，则返回错误。

## `ComponentEffects`

```rust
#[derive(Clone, Debug)]
pub struct ComponentEffects { /* effects: EffectSet */ }
impl ComponentEffects {
    pub fn new(effects: EffectSet) -> Self;
    pub fn effect_set(&self) -> &EffectSet;
}
```

组件成功 apply 后保留的 effect。`apply` 返回它；supervisor 把它作为 fiber 的 cleanup 存储，并在
fiber unload 时（按 LIFO）将其 dispose。

## `DependencySet`

```rust
pub trait DependencySet: Send + Sync + 'static {
    fn injects() -> Vec<InjectSpec>;
    fn resolve(resolver: &dyn DependencyResolver) -> Result<Self, CordisError>
    where
        Self: Sized;
}
```

native 组件或注入方法的编译期依赖声明。`injects()` 返回已声明的 `InjectSpec`；`resolve()` 依据
committed 依赖视图构建生成的类型化 client 集。生成的 `CounterConsumerDependencies` 实现了它。

**错误** —— 当选中的提供者（provider）没有匹配的 dispatcher 时，`resolve` 返回错误。

## `NoDependencies`

```rust
#[derive(Clone, Copy, Debug, Default)]
pub struct NoDependencies;
impl DependencySet for NoDependencies { /* injects() -> vec![], resolve() -> Ok(Self) */ }
```

为没有 inject 的组件准备的依赖集。

## `DependencyResolver`

```rust
pub trait DependencyResolver {
    fn resolve(&self, service: &ServiceId) -> Result<Arc<dyn ServiceDispatcher>, CordisError>;
}
```

解析某个已提交的（committed）fiber 加载 epoch 所选中的 dispatcher。给定该服务，它返回 committed
view 中选定提供者的 `Arc<dyn ServiceDispatcher>`。

**错误** —— 当 committed view 没有提供者，或 native 提供者没有注册其 dispatcher 时返回错误。

## `MethodContext<D>` 与 `MethodFiberRuntime`

```rust
#[derive(Clone, Debug)]
pub struct MethodContext<D: DependencySet> { /* context, deps: Arc<D>, effects */ }
impl<D: DependencySet> MethodContext<D> {
    pub fn context(&self) -> &Context;
    pub fn deps(&self) -> &D;
    pub fn effects(&self) -> &EffectSet;
    pub fn effect_set(&self) -> EffectSet;
}

#[derive(Clone, Debug)]
pub struct MethodFiberRuntime { /* runtime: RuntimeHandle, services: NativeServiceRegistry */ }
impl MethodFiberRuntime {
    pub fn new(runtime: RuntimeHandle, services: NativeServiceRegistry) -> Self;
    pub fn services(&self) -> &NativeServiceRegistry;
}
```

- `MethodContext` 是传给某个方法级注入 child fiber 的 context。访问器与 `ComponentContext` 相同，
  只少 `register_method`。
- `MethodFiberRuntime` 是生成的方法级注入所用的运行时桥梁：它携带 `RuntimeHandle`（用于创建
  child fiber、提交依赖、安装 executor）与 `NativeServiceRegistry`（用于解析 native dispatcher）。

## `NativeServiceRegistry`

```rust
#[derive(Clone, Default)]
pub struct NativeServiceRegistry { /* dispatchers: Arc<RwLock<BTreeMap<(FiberId, ServiceId), Arc<dyn ServiceDispatcher>>>> */ }
impl NativeServiceRegistry {
    pub fn new() -> Self;
    pub fn insert(&self, provider: FiberId, dispatcher: Arc<dyn ServiceDispatcher>);
    pub fn remove(&self, provider: FiberId, service: &ServiceId);
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}
```

以 provider fiber 为键的 native 服务 dispatcher。native 提供者通过 `insert` 在这里注册其
dispatcher，因此生成的依赖 client 可以按 `(provider, service)` 解析到它。

**错误** —— 当该组合没有注册 dispatcher 时，内部 `resolve` 返回
`CordisError::MissingServiceDispatcher { provider, service }`。

## `MethodFiberRuntime` + 依赖解析流程

`DependencySet::resolve` 会收到一个 `dyn DependencyResolver`。生成的组件 adapter 使用 committed
view 加 native registry：

```rust
struct CommittedDependencyResolver<'a> { committed: &'a CommittedView, services: &'a NativeServiceRegistry }
impl DependencyResolver for CommittedDependencyResolver<'_> {
    fn resolve(&self, service: &ServiceId) -> Result<Arc<dyn ServiceDispatcher>, CordisError> {
        let provider = self.committed.lookup(service)?.ok_or(CordisError::MissingCommittedProvider { service: service.clone() })?;
        self.services.resolve(provider, service)
    }
}
```

## 隐藏的内部实现

这些在源码中是 `#[doc(hidden)]`——是真实公开的 API，但面向宏生成的代码，而不是供直接使用。组件
作者永远不会指名它们。

### `ComponentCell<T>`

```rust
#[doc(hidden)]
pub struct ComponentCell<T> { /* inner: Arc<AsyncMutex<T>> */ }
impl<T> ComponentCell<T> {
    pub fn new(value: T) -> Self;
    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, T>;
}
```

生成组件 adapter 使用的共享、异步串行化的所有权。`apply` 方法把组件实例锁在这个 cell 之后，使
方法注册与 `apply` 主体共享唯一的 `&mut self` 所有者，而无数据竞争。

### `catch_component_future`

```rust
#[doc(hidden)]
pub async fn catch_component_future<T, F>(fiber: FiberId, future: F) -> Result<T, CordisError>
where F: Future<Output = Result<T, CordisError>> + Send;
```

把生成组件代码中的 panic 转换为 fiber 失败。把 future 包装进
`AssertUnwindSafe(...).catch_unwind()`；panic 会变成 `CordisError::FiberExecutorPanicked { fiber, message }`。

## `config_schema`

```rust
pub fn config_schema<T: JsonSchema>() -> Schema
```

生成一个 Draft 2020-12 JSON Schema，无需宏指名 schemars 的内部细节。

## 示例

完整的 native 路径（`crates/cordis/examples/native_counter.rs`），含 `#[cordis::component]`、
`#[cordis::component_impl]` 与一个 `CounterClient::from_native` 依赖，展示在
[macros](macros.zh.md) 中。`counter-consumer` 就是该组件；它的 `apply` 读取
`context.deps().counter` 与 `config.amount`。
