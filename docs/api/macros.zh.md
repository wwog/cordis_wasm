# 宏

Cordis 提供六个过程宏（procedural macro）。它们把一个普通 trait 或 struct 变成运行时服务规格
（runtime-service specification）、事件规格（event specification），或原生组件——其依赖图、
config schema 与 effect 生命周期都被运行时理解。六个宏都在 `cordis-macros` 中，并通过 `cordis`
facade 重新导出，因此你写 `#[cordis::service]` 等即可，永远不必指名宏 crate。

它们生成的编译期身份是整个模型的核心：每个服务（service）与事件（event）都会得到一个 **name**，
外加一个由 BLAKE3 对规范化签名（canonical signature）计算得出的 32 字节 ABI 哈希。两个同名但
方法签名不同的声明会得到不同的哈希，因此不会互相满足。

## `#[cordis::service(name = "...")]`

应用于一个 `trait`。声明一个*服务（service）*——一个具名、可远程调用的 RPC 表面。

- 参数：`name`（一个字符串）。可选；默认取 trait 标识符。
- trait 体内只能包含 `async fn` 方法。每个方法必须接收 `&self`、接收拥有的参数（owned arguments），
  并返回 `Result<T, E>`。不允许默认实现、泛型或 `where` 子句。参数必须是普通标识符（不能有 `ref`、
  `mut` 或解构）。参数必须为拥有的（owned），因为 native 与 Wasm 分发使用同一条 wire ABI。

```rust
use cordis::ServiceCallError;

#[cordis::service(name = "example.counter")]
pub trait Counter {
    async fn add(&self, amount: i64) -> Result<i64, String>;
}
```

宏把每个方法的签名改写为

```rust
fn add(&self, amount: i64) -> impl std::future::Future<Output = Result<i64, String>> + Send;
```

于是该 trait 变成 object-safe，可被具体提供者（provider）实现。

### 生成的伴生类型

对 `trait Counter`，宏生成以下内容（均采用该 trait 的可见性）：

| Item | Purpose |
|---|---|
| `struct CounterService;` | 零大小的 **marker**。实现 `ServiceKey`（`NAME`、`ABI_HASH`）与 `ServiceSpec`。 |
| `struct CounterClient;` | 类型化的 **client**。通过 `ServiceDispatcher` 向提供者发送调用。 |
| `struct CounterDispatcher<T>;` | 包装 `Arc<T>` 提供者实现的 **object-safe dispatcher**。实现 `ServiceDispatcher`。 |

`CounterClient` 有三个公开构造器和一个访问器：

```rust
impl CounterClient {
    // Checks the dispatcher's ServiceId (name + hash) before use.
    pub fn new(dispatcher: Arc<dyn ServiceDispatcher>) -> Result<Self, CordisError>;

    // Zero-serialization direct path, used by the native_counter example.
    pub fn from_native<T: Counter + Send + Sync + 'static>(service: Arc<T>) -> Self;

    pub fn service_id(&self) -> &ServiceId;
}
```

`CounterClient::new` 首先调用 `ServiceClient::new::<CounterService>(dispatcher)`；当 dispatcher 的
name 或哈希不匹配时，该方法返回 `CordisError::ServiceIdentityMismatch`。

与 spec 一样，client 方法都是 `async` 的并返回 `Result<T, ServiceCallError<E>>`。动态分支用
`encode_service_payload` 对参数编码，调用 `counter.call(method_id, payload)`，再解码响应
`Result<T, E>`；传输失败以 `ServiceCallError::Transport` 呈现，提供者失败以
`ServiceCallError::Service` 呈现。

`CounterDispatcher::new(service)` 包装一个 `Arc<T>`；`dispatch` 对 method id 做匹配，遇到未知的
id 时返回 `CordisError::UnknownServiceMethod`。

## `#[cordis::event(name = "...", mode = "...")]`

应用于一个 `trait`。声明一个*事件（event）*——具有固定语义的分发表面。该 trait 只能声明两个带值的
关联类型 `type Input` 与 `type Output`。

- 参数：`name`（字符串，默认取 trait 标识符）与 `mode`（`emit`、`parallel`、`serial`、`bail`、
  `waterfall` 之一；默认 `parallel`）。
- 当 `mode = "waterfall"` 时，`Input` 与 `Output` 必须是相同类型。

```rust
#[cordis::event(name = "app.counter", mode = "serial")]
pub trait CounterChanged {
    type Input = u64;
    type Output = ();
}
```

### 生成的伴生类型

宏生成一个 marker `CounterChangedEvent`（带 `NAME`、`ABI_HASH`、`MODE`、`runtime()`，以及
`encode_input`/`decode_input`/`encode_output`/`decode_output`），为它实现 `EventSpec`，并生成一个
持有对应 mode 的运行时类型的类型别名 `CounterChangedRuntime`：

| Mode | Runtime type |
|---|---|
| `emit` / `parallel` / `serial` | `AsyncEvent<Input, Output>` |
| `bail` | `BailEvent<Input, Output>` |
| `waterfall` | `WaterfallEvent<Input>` |

marker 还有一个 `dispatch` 关联函数。它的签名取决于 mode（各 mode 的语义见
[event](event.zh.md)）：

```rust
// emit: fire-and-forget, errors go to an error sink
fn dispatch<S>(event, target, input, error_sink) -> Result<(), CordisError> where S: Fn(CordisError) + Send + Sync + 'static;

// parallel
async fn dispatch(event, target, input) -> Result<Vec<ControlFlow<Output>>, CordisError>;

// serial / bail
// bail is sync: fn ... -> Result<Option<Output>, CordisError>
async fn dispatch(event, target, input) -> Result<Option<Output>, CordisError>;

// waterfall
async fn dispatch(event, target, input) -> Result<Output, CordisError>;
```

## `#[cordis::component(name = "...", config = "...")]`

应用于一个 `struct`。声明一个*原生组件（native component）*：它的 config 类型、被注入的依赖以及
它的 descriptor。与同一 struct 上的 `#[cordis::inject(...)]` 配合使用。

- 参数：`name`（字符串，默认取 struct 标识符）与 `config`（一个类型，默认为 `()`）。config 类型
  必须实现 `DeserializeOwned + JsonSchema + Send + Sync + 'static`。
- `#[cordis::inject(ServiceA, ServiceB, ...)]` 列出该组件所依赖的服务。每一项都是一个服务的
  **trait** 路径；宏从它派生出 marker（`ServiceAService`）。

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CounterConfig { amount: i64 }

#[derive(Debug)]
#[cordis::component(name = "counter-consumer", config = CounterConfig)]
#[cordis::inject(Counter)]
pub struct CounterConsumer;
```

对 `struct CounterConsumer`，宏生成：

- `struct CounterConsumerDependencies`：每个 inject 一个字段，按 service trait 的 snake-case 命名
  （`counter: CounterClient`），外加一个 `new(...)` 构造器。它实现 `DependencySet`；其 `injects()`
  返回 `InjectSpec`，`resolve(resolver)` 依据 committed view 构建 client。
- `impl ComponentDefinition for CounterConsumer`：带 `type Config = CounterConfig`、
  `type Deps = CounterConsumerDependencies`，以及一个返回 `ComponentDescriptor { name, injects, config_schema }`
  （缓存在 `OnceLock` 中）的 `descriptor()`。

## `#[cordis::component_impl]`

应用于指向组件 struct 的 `impl` 块。把它变成一个 `Component`。它要求：

- 恰好一个标记了 `#[cordis::apply]` 的方法，签名为
  `async fn name(&mut self, context: ComponentContext<XxxDependencies>, config: XxxConfig) -> Result<(), CordisError>`。
  这就是该组件的 *effect body*：在 context 上注册 listener/provider 后返回；失败会拆除这些 effect
  并让 fiber 失败。
- 零个或多个形如
  `async fn name(&mut self, method_context: MethodContext<...>) -> Result<(), CordisError>` 的
  `#[cordis::inject(...)]` 方法。每个方法都成为该组件 effect 树所拥有的独立
  *方法级子 fiber（method-level child fiber）*（见
  [native-component](native-component.zh.md#methodcontextd-与-methodfiberruntime)）。

```rust
#[cordis::component_impl]
impl CounterConsumer {
    #[cordis::apply]
    async fn start(
        &mut self,
        context: ComponentContext<CounterConsumerDependencies>,
        config: CounterConfig,
    ) -> Result<(), cordis::CordisError> {
        let value = context.deps().counter.add(config.amount).await
            .map_err(service_error)?;
        println!("counter value: {value}");
        Ok(())
    }
}
```

生成的 `Component::apply` 在 `catch_component_future` 内部先运行方法级注册，再运行 `apply` 方法。
出错时它会 dispose 该 effect set 并返回错误；成功时返回 `ComponentEffects`，保留 effect set，以便
supervisor 在 fiber unload 时将其 dispose。

## `#[cordis::inject(...)]`

inject 属性在两处有意义，并由两个不同的宏处理：

- 在 `#[cordis::component]` struct 上：声明该组件所需的服务。
- 在 `#[cordis::component_impl]` 内的一个方法上：声明该方法自身的依赖，并使其成为一个
  child fiber，通过 `MethodContext` 访问这些依赖。

其形式为 `#[cordis::inject(Service, OtherService)]`——用逗号分隔的 trait 路径列表，其中每一项都
必须是一个 `ServiceSpec`。

## `#[cordis::apply]`

这是一个 marker，不是转换。它标记 `#[cordis::component_impl]` 块中作为组件 effect body 的那一个
`async fn`。展开后该块的 `Component::apply` 会调用它，并且恰好需要一个。

## ABI 哈希身份

`#[cordis::service]` 依据服务 `name` 加上每个方法的规范化签名来计算其哈希。方法的规范化形式为：

```text
method_name(arg_type1,arg_type2,...)->Result<OkType,ErrType>
```

服务哈希是对 `name` 后接各规范化形式（**无条件排序**）做 BLAKE3 的结果（因此方法声明顺序不影响
wire protocol）。每个方法还会得到一个 `method_id`，它是对 `name\ncanonical_method` 做 BLAKE3
所得结果的前四个字节（小端序）。

`#[cordis::event]` 对以下内容计算哈希

```text
name\nmode\n<Input type>\n<Output type>
```

因此 mode 参与一个事件的身份，并且只有类型（而非参数名）起作用。

这就是论文的 key-namespacing 路线（§6.6）：来自某个模块的名为 `example.counter` 的服务永远不会
满足期望不同 `example.counter` ABI 的消费者，即使名称相同——因为哈希不同。

## 完整可运行示例

完整的 native 路径，摘自 `crates/cordis/examples/native_counter.rs`：

```rust
use cordis::{Component, ComponentContext, Context, EffectSet, Runtime, ServiceCallError};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

#[cordis::service(name = "example.counter")]
pub trait Counter {
    async fn add(&self, amount: i64) -> Result<i64, String>;
}

#[derive(Debug, Default)]
struct AtomicCounter { value: AtomicI64 }

impl Counter for AtomicCounter {
    fn add(&self, amount: i64) -> impl Future<Output = Result<i64, String>> + Send {
        let result = self.value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(amount)
        }).map(|previous| previous + amount).map_err(|_| "counter overflow".to_owned());
        std::future::ready(result)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CounterConfig { amount: i64 }

#[derive(Debug)]
#[cordis::component(name = "counter-consumer", config = CounterConfig)]
#[cordis::inject(Counter)]
pub struct CounterConsumer;

#[cordis::component_impl]
impl CounterConsumer {
    #[cordis::apply]
    async fn start(
        &mut self,
        context: ComponentContext<CounterConsumerDependencies>,
        config: CounterConfig,
    ) -> Result<(), cordis::CordisError> {
        let value = context.deps().counter.add(config.amount).await.map_err(service_error)?;
        println!("counter value: {value}");
        Ok(())
    }
}

fn service_error(error: ServiceCallError<String>) -> cordis::CordisError {
    match error {
        ServiceCallError::Transport(error) => error,
        ServiceCallError::Service(message) => cordis::CordisError::SupervisorFailed { message },
    }
}

#[tokio::main]
async fn main() -> Result<(), cordis::CordisError> {
    let runtime = Runtime::start();
    let fiber = runtime.handle().create_fiber(None).await?;
    let counter = Arc::new(AtomicCounter::default());
    let client = CounterClient::from_native(counter);
    let context = ComponentContext::new(
        Context::root(fiber),
        CounterConsumerDependencies::new(client),
        EffectSet::new("counter-consumer"),
    );
    let effects = CounterConsumer.apply(context, CounterConfig { amount: 3 }).await?;
    effects.effect_set().dispose().await.map_err(|error| cordis::CordisError::DisposerFailed {
        message: error.to_string(),
    })?;
    runtime.shutdown().await?;
    Ok(())
}
```

用 `cargo run -p cordis --example native_counter` 运行它；它会打印 `counter value: 3`。

## 编译期错误

宏在 `cargo build` 时报告契约违规（contract violation）错误。其中有意义的如下：

- 带泛型参数的 service/event trait。
- 服务方法不是 `async`、接收 `&mut self`、带默认实现体、是泛型、含 `where` 子句、使用引用参数，
  或不返回 `Result<T, E>`。
- event trait 声明了除 `type Input`/`type Output` 以外的任何内容，或 `Input` 与 `Output` 不同的
  `waterfall` 事件。
- 在外部（foreign）trait 的 impl 上使用 `#[cordis::component_impl]`、存在多个 `#[cordis::apply]`
  方法、`apply` 方法的参数个数/类型错误，或 `apply` 与 `inject` 混用。
- 参数个数错误或缺 `&mut self` 的方法级 inject。
- 重复的服务方法 id，或注入的服务没有各异的 client 字段名。
