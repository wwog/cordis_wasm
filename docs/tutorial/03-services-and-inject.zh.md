# 3. 服务与注入

**服务（service）**是一个插件提供、其他插件消费的具名能力。counter 示例是两个插件加一个服务：提供者拥有 `example.counter`，消费者调用它。本章解释服务身份如何运作、为什么它携带 32 字节的哈希、一项服务如何从"在 `activate` 中提供"变成"可按名称路由"，以及 `cordis.json` 中的 `isolate` 映射如何决定消费者看到哪个提供者。本章也会对比 guest 手工编写的注册与把这一切都封装起来的 native 宏路径。

## `ServiceId { name, abi_hash }`

一个服务身份由两个字段组成：

```wit
record service-id {
    name: string,
    abi-hash: list<u8>,
}
```

在 guest 侧，它们变成你直接构造的值：

```rust
const COUNTER_ABI: [u8; 32] = [0x43; 32];

fn counter_service() -> ServiceId {
    ServiceId {
        name: "example.counter".into(),
        abi_hash: COUNTER_ABI.to_vec(),
    }
}
```

`name` 是每个应用单个扁平命名空间里的一个字符串。counter 示例用点号约定 `example.counter` 来做命名空间，但没有任何机制强制唯一性。消费者与提供者都定义*相同*的 `counter_service()` 函数，使名称与哈希对齐。

### ABI 哈希为何存在

单凭 `name` 不足以判定两个组件对同一服务协议达成一致。两个插件可能都把一项服务命名为 `example.counter`，却意指完全不同的东西——一个暴露 `get() -> u64`，另一个暴露 `increment(delta: u32) -> Result<(), E>`。如果运行时只按名称匹配，针对前者编译的消费者会把调用静默路由给后者，并以令人困惑的方式失败。

哈希解决歧义。在 native 路径中，`#[cordis::service]` 对由服务名加上每个方法的 *canonical 签名*拼接而成的 canonical 字符串计算 **BLAKE3** 摘要。请看 `crates/cordis-macros/src/lib.rs` 里的 `service_abi_hash`：

```rust
fn service_abi_hash(name: &str, methods: &[ServiceMethod]) -> [u8; 32] {
    let mut canonical = String::from(name);
    let mut signatures = methods
        .iter()
        .map(|method| canonical_service_method(&method.method, &method.arguments, &method.ok, &method.error))
        .collect::<Vec<_>>();
    signatures.sort_unstable();
    for signature in signatures {
        canonical.push('\n');
        canonical.push_str(&signature);
    }
    hash_text(&canonical)
}
```

README 精确陈述了这个性质：哈希只由服务名、方法名、按顺序排列的参数类型与返回类型导出。注释、参数改名与方法声明顺序都**不影响**它。因此只要线契约（wire contract）一致，两个独立编写的消费者与提供者会得到相同的哈希，而任何不匹配都会在任何调用被路由之前被捕获。

guest 路径并不运行这个宏——它提供一个硬编码的 `[u8; 32]` 常量。这就是约定：你作为插件作者，负责选一个与你正在讲的协议相匹配的哈希。实践中，一个协议的两端会定义一个共享常量（就像两个 counter crate 用 `COUNTER_ABI = [0x43; 32]` 那样）；真正的 `cordis` 服务宏可以从 Rust 类型算出它，然后你把得到的字节复制进 guest。

### host 如何匹配哈希

host 在 `crates/cordis-wasm/src/runtime.rs` 中把 WIT 的 `list<u8>` 与它内部的 `[u8; 32]` 相互转换：

```rust
fn hash_from_bytes(bytes: &[u8]) -> Result<[u8; 32], WasmHostError> {
    <[u8; 32]>::try_from(bytes).map_err(|_| WasmHostError::Descriptor {
        message: "service ABI hash must contain 32 bytes".to_owned(),
    })
}
```

这是严格的转换。如果 guest 发送的哈希不是恰好 32 字节，加载会在任何激活之前以 `Descriptor` 错误失败。一旦转换完成，内部 `ServiceId` 在**两个**字段上都 derive `Eq` 与 `Ord`，所以两个 id 只有在 name *和* hash 都匹配时才相等。supervisor 按 `ServiceId` 查找提供者——因此提供者只有在完整身份对齐时才满足消费者。

## 服务如何变得可路由

提供者并不直接*暴露*方法。它在 `activate` 期间调用 `provide_service`，其余由 host 完成：

**Guest 侧**（提供者）：

```rust
fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let registration = host::provide_service(context, &counter_service())?;
    REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
    Ok(())
}
```

**Host 侧** —— `GuestState::provide_service` → `add_registration` → `InstanceHost::register`，最终到达 `crates/cordis-wasm/src/loader.rs` 中的 `RuntimeKernel::provide_service`：

```rust
fn provide_service(&self, fiber: FiberId, key: ProviderKey, scope: EffectScope) -> ComponentFuture<'_, ()> {
    Box::pin(async move {
        self.runtime.provide(key.clone(), fiber).await?;
        // register a disposer that withdraws on teardown ...
    })
}
```

`runtime.provide(key, fiber)` 记录 `fiber` 在该 realm 中提供此服务。从那一刻起，任何 committed view 解析到该提供者的消费者都可以调用它。注册是一个 **effect**：host 注册一个在 fiber 卸载时调用 `runtime.withdraw` 的 `Disposer`，因此服务随其提供者一起消失。

`provide_service` 返回给 guest 的是一个 `Registration` 句柄。host 是清理的权威——即使 guest 丢弃该句柄也不会让 effect 失效，因为 disposer 归 fiber 的 `EffectGuard` 所有。这正是 README 里"guest 不可信"的假设，也是为什么 guest 侧把注册存进 `thread_local!` 的模式关乎*排序停用*，而非*保证*清理。

## `call_service` —— 消费者侧

消费者在 `activate` 期间调用服务：

```rust
fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
    let _: u64 = cordis_guest::call_service(&context, &counter_service(), GET_METHOD, &1_u64)?;
    Ok(())
}
```

`cordis_guest::call_service` 是 host import 之上的类型化包装：

```rust
pub fn call_service<Req, Res>(context: &host::CallContext, service: &host::ServiceId, method: u32, request: &Req) -> Result<Res, host::KernelError> {
    let payload = encode(request)?;
    let reply = host::call_service(*context, service, method, &payload)?;
    decode(&reply)
}
```

它把 `Req` 编码成 MessagePack、调用 host import、再解码 `Res`。在 host 上，这个 import（`GuestState::call_service`）做的正是你所期望的路由：

1. 校验 context 与 payload limits。
2. 用 `runtime.commit_dependencies(fiber)` 解析*当前* fiber 的 committed 依赖视图。
3. 在该视图中查找服务的提供者 —— `committed.lookup(&call.service)`。
4. 通过 `self.route(provider).await?.call_service(call)` 把调用路由给提供者 fiber。

关键细节是**第 2 步**。调用穿过调用方的 committed view 路由，而不是按全局名称路由。消费者只能触达它在 `inject` 中声明的服务，而且只能触达它的 *context* 解析出的那个提供者。这正是 realm 布线（下文）生效的原因：路由是调用方 context 的函数，而非仅仅服务名的函数。

## Realms：`isolate` 如何连通消费者与提供者

重读 `examples/wasm-app/cordis.json` 的相关部分：

```json
{ "id": "consumer", "component": "file:...", "config": {}, "isolate": { "example.counter": "example" } },
{ "id": "provider", "component": "file:...", "config": {}, "isolate": { "example.counter": "example" } }
```

两个条目都把服务 `example.counter` 映射到 realm 标签 `example`。loader 在 `WasmEntryDriver::entry_context` 中收集 descriptor 的 inject 与 provide 的并集，并对每一项调用 `realm_for`：

```rust
async fn realm_for(&self, entry: &ResolvedEntry, service: &ServiceId) -> Result<RealmId, LoaderError> {
    let key = match entry.realms.get(service.name()) {
        Some(ManagedRealm::Local { owner, service }) => RealmKey::Local(owner.clone(), service.clone()),
        Some(ManagedRealm::Global { label, service }) => RealmKey::Global(label.clone(), service.clone()),
        None => RealmKey::Default(service.clone()),
    };
    ...
}
```

`isolate` 映射中的值决定 realm key：

- 一个裸 **字符串**（`"example"`）→ 一个**全局** realm，由每个指名该标签的条目共享；
- **`true`** → 一个**本地** realm，只作用域到该条目及其后代；
- **缺失** → 该服务的**默认** realm。

两个条目都把 `example.counter` 解析到同一个全局 realm key，所以两者得到同一个 `RealmId`。提供者把服务注册进那个 realm；消费者的 committed view 解析到它。

这就是为什么**两个**条目都必须携带该映射。如果消费者省略它，`example.counter` 会在默认 realm 中解析——那里没有提供者注册——于是它的 fiber 将永远停在 `Pending`（经典的"永不激活"症状，见第 8 章）。如果提供者省略它，提供者会注册进默认 realm，而消费者在 `example` 中查找，两者永不碰面。

Realms 是运行时的隔离机制：把同一服务的两个提供者放进不同 realm，每个消费者只会看到它的 context 指名的那个。`"example"` 标签被两个条目共享，正是为了让它们*确实*碰面。论文把这称为 realm isolation；本教程称它为"两个条目就服务所在位置达成一致"。

## 构建时 vs 运行时服务路由

一项服务的生命周期中有两个截然不同的时刻，值得给它们命名：

- **构建（加载）时。** descriptor 的 `provide` 与 `inject` 列表在组件加载时被读取。`provide` 播种可能在此路由的服务集合；`inject` 播种决定 fiber 能否激活的依赖解析。两者此时都不做任何实际路由——它们是*声明出来的*意图。

- **运行时（调用）时。** `call_service` 与 `provide_service`/`register_listener` 发生在 fiber 活跃时。`provide_service` 注册进一个 realm；`call_service` 解析调用方的 committed view 并路由到特定的提供者 fiber。**committed view** 是关键：它是消费者激活那一刻冻结的提供者选择，记录为一个 `FiberId`，而非一个值。如果提供者之后被替换，supervisor 会重算受影响的消费者，它们针对新提供者重载——committed view 随之改变。

实践规则：组件*声明的* `provide` 列表使它成为候选者；它在 `activate` 中实际的 `provide_service` 调用使它成为某个 realm 中*当前的*提供者。一个 descriptor 在 `provide` 里声明了服务却从不调用 `provide_service`，将"原则上可路由"但"实际上不存在"，等待它的消费者保持 `Pending`。

## 对照：native `#[cordis::service]` 宏路径

以上所有内容描述的都是 **guest**，它手工完成 kernel 握手。native 路径刻意自动化得多、远不那么手动。想想宏为服务 trait 生成了什么（参见 `crates/cordis-macros/src/lib.rs` 与 `crates/cordis-core/src/native.rs`）：

```rust
// You write:
#[cordis::service]
trait Counter {
    async fn get(&self, key: u64) -> Result<u64, CordisError>;
}
```

宏把它展开成若干生成的条目，包括：

- 一个 **marker** 类型，实现带计算出的 `NAME` 与 `ABI_HASH` 的 `ServiceKey`；
- 一个带 `service_id()` accessor、每个 trait 方法一个生成方法的 **client** 结构体；
- 两个构造函数：
  - `new(Arc<dyn ServiceDispatcher>)` —— 动态路径，在验证 dispatcher 的 service id **与期望 id 匹配**之后，把它包装进 `ServiceClient`。这正是 README 称为 MessagePack 动态路径的那个 `ServiceClient::new::<S>(dispatcher)`，也是 Wasm 边界复用的那条路径。
  - `from_native(Arc<T>)`，其中 `T: Counter` —— 零序列化的快速路径，直接把具体服务包装进 object-safe adapter。注意 README：`from_native` 是 native 静态快速路径，`new` 是动态路径。
- 一个实现 `ServiceDispatcher` 的 **dispatcher** 结构体，使 host 能泛型地路由到它。

在 native 侧，组件通过生成的 `DependencySet` 获取依赖：

```rust
impl ::cordis::DependencySet for CounterDependencies {
    fn injects() -> Vec<InjectSpec> {
        vec![InjectSpec::required(Counter::service_id())]
    }
    fn resolve(resolver: &dyn DependencyResolver) -> Result<Self, CordisError> {
        Ok(Self::new(CounterClient::new(resolver.resolve(&Counter::service_id())?)?))
    }
}
```

所以 native 路径**生成** ABI 哈希、生成 client、生成依赖解析。guest 手工完成的胶水——用匹配的哈希构造 `ServiceId`、以正确的方法 id 调用 `call_service`、把 `Registration` 存进 thread-local——在 native 侧是宏的职责。

一句话对比：**native 宏生成身份、client 与依赖解析；guest 三者全靠手写。** 仓库今天的立场是，guest 是更底层、更显式的路径——counter 示例是它的模板——而宏路径是嵌入型 host 为 native 组件所用的路径。第 6 章的 web-server 插件恰恰是你会感受到这个差距的那种插件：你得自己计算方法 id 和哈希，或复用 host 侧能识别的常量。

### 为何 guest 今天用手写

目前没有一个 `#[cordis::service]` 的等价宏，能为一个 `wasm32-wasip2` guest 生成带匹配哈希的 WIT `service-id`。`cordis-guest` crate 刻意保持最小：它生成 kernel 绑定和少量 helper，但不生成服务派生层。所以一个 guest 插件要么：

- 从协议的事实源（source of truth）复制常量哈希（如 counter crates 所做），要么
- 使用一个 **host** 构建时就用过的哈希的服务 id。

无论哪种方式，哈希都是你 guest 中一个固定的 `[u8; 32]`，由你负责让两端保持一致。这是一个值得直说的真实边界：它不是 bug，而是 guest SDK 的现状，也是你若扩展它时首先会碰触的地方之一。

## 服务生命周期速览

| 时刻 | 发生什么 | 位置 |
|---|---|---|
| 加载 | 读取 descriptor；`inject`/`provide` 列表成为依赖解析输入 | `WasmComponentFactory::from_bytes` → `descriptor_from_wit` |
| 激活 | `provide_service` 把提供者注册进它的 realm；effect 拥有清理 | Guest `activate` → `GuestState::provide_service` → `RuntimeKernel::provide_service` |
| 调用 | 调用方的 committed view 解析出提供者 fiber；调用被路由给它 | `GuestState::call_service` → `RuntimeKernel::call_service` |
| 卸载 | Disposer 调用 `withdraw`；提供者从它的 realm 中移除 | `RuntimeKernel::provide_service` scope defer |

下一篇：[事件](04-events.zh.md) —— WIT 事件面、五种 `EventMode`、`EventReply` 与监听器注册。
