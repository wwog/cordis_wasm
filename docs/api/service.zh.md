# 服务

服务子系统是 Cordis 的 RPC 边界。**服务（service）**是 `#[cordis::service]` 声明为 trait 的具名、
类型化表面。Native 与 WebAssembly 组件共享同一个 `ServiceId`、同一个 `ServiceDispatcher`
transport、以及同一个 `MessagePack` payload codec——正因如此，native provider 才能服务 Wasm
consumer，反之亦然。服务同时也是**响应式依赖注入**的单位：组件声明它想要哪些服务，并且只有当
它们全部被提供后，组件才会激活。

## `ServiceId`

```rust
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ServiceId { /* name: Arc<str>, abi_hash: [u8; 32] */ }

impl ServiceId {
    pub fn new(name: impl Into<Arc<str>>, abi_hash: [u8; 32]) -> Self;
    pub fn of<K: ServiceKey>() -> Self;
    pub fn name(&self) -> &str;
    pub const fn abi_hash(&self) -> &[u8; 32];
}
```

所有组件类型共享的稳定服务标识。一个 `ServiceId` 由**名称（name）**加**32 字节 ABI hash** 组成。
两个同名但签名不同的服务会得到不同的 hash，因此互不满足对方——这正是论文的 key-namespacing
路线（§6.6）。

`Display` 的格式是 `{name}@{first-4-bytes-of-hash-as-hex}`（例如 `test.service@abababab`），与
测试 `typed_identity_contains_name_and_abi_hash` 一致。

## `ServiceKey`

```rust
pub trait ServiceKey: Send + Sync + 'static {
    const NAME: &'static str;
    const ABI_HASH: [u8; 32];
}
```

由 `#[cordis::service]` 生成的编译期服务标识。标记类型（例如 `CounterService`）实现它；它是你
交给 `ServiceSpec::service_id` 与 `ServiceClient::new` 的零尺寸 token。

## `ServiceSpec`

```rust
pub trait ServiceSpec: ServiceKey {
    fn service_id() -> ServiceId
    where
        Self: Sized,
    {
        ServiceId::of::<Self>()
    }
}
impl<T: ServiceKey> ServiceSpec for T {}
```

对每个 `ServiceKey` blanket 实现：方便地给它们一个 `service_id()`。宏生成的标记实现它。

## `ServiceDispatcher`

```rust
pub trait ServiceDispatcher: Send + Sync + 'static {
    fn service_id(&self) -> ServiceId;
    fn dispatch(&self, method_id: u32, payload: Vec<u8>) -> ServiceFuture;
}
```

**object-safe 的 transport 边界**。这正是宏的 `CounterDispatcher<T>` 与 Wasm runtime 都会实现的
东西，因此在调用方看来，native 与 dynamic client 可以互换。`service_id` 声明此 dispatcher 提供
哪个服务；`dispatch` 路由一次已编码的 method 调用。`dispatch` 返回的任何东西都是 *wire* 响应——
调用方一侧的 codec 调用（`decode_service_payload`）把它转成类型化结果。

## `ServiceClient`

```rust
#[derive(Clone)]
pub struct ServiceClient { /* service: ServiceId, dispatcher: Arc<dyn ServiceDispatcher> */ }

impl ServiceClient {
    pub fn new<S: ServiceSpec>(dispatcher: Arc<dyn ServiceDispatcher>) -> Result<Self, CordisError>;
    pub fn service_id(&self) -> &ServiceId;
    pub async fn call(&self, method_id: u32, payload: Vec<u8>) -> Result<Vec<u8>, CordisError>;
}
```

由生成的 client 内部使用的、带校验的 type-erased transport。`new` 在返回前把 dispatcher 的
`ServiceId`（名称与 ABI hash）对照期望的 spec 校验。

**错误** —— 当 dispatcher 没有实现所请求的 service ABI 时，`new` 返回
`CordisError::ServiceIdentityMismatch { expected, actual }`。`call` 返回 dispatcher 报告的
transport 或 provider 错误。

## `ServiceFuture`

```rust
pub type ServiceFuture =
    Pin<Box<dyn Future<Output = Result<Vec<u8>, CordisError>> + Send + 'static>>;
```

由 native 或 WebAssembly service dispatcher 返回的 owned future。

## `ServiceCallError<E>`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceCallError<E> {
    Transport(CordisError),
    Service(E),
}
impl<E> From<CordisError> for ServiceCallError<E> { ... }
```

每个生成的 client method 返回的错误类型。它把 **transport**（codec、路由、ABI 不匹配——Cordis
机制中的任何东西）与**服务自身声明的错误** `E` 分开。`From<CordisError>` 把 transport 失败转成
`ServiceCallError::Transport`，因此 client-builder 内部的 `?` 会传播 transport 这一半。它实现了
`Display` 与 `std::error::Error`（`source` 返回内部错误）。

## `ProviderKey`

```rust
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProviderKey {
    pub service: ServiceId,
    pub realm: RealmId,
}
impl ProviderKey {
    pub const fn new(service: ServiceId, realm: RealmId) -> Self;
}
```

一个 service provider 占据的唯一槽位。给定的一对 `(service, realm)` 至多由一个 fiber 提供。用
**service + realm** 唯一标识 provider，是论文 §6.2 中服务复用（service multiplexing）的 "realms"
路线（另一条路线、exclusive binding，就是 `set` 连同其 `DuplicateProvider` 前置条件）。

## `InjectSpec`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InjectSpec {
    pub service: ServiceId,
    pub requirement: Requirement,
}
impl InjectSpec {
    pub const fn required(service: ServiceId) -> Self;
    pub const fn optional(service: ServiceId) -> Self;
}
```

组件声明的一个依赖。`required` 依赖在 provider 出现之前阻止 fiber 激活；`optional` 依赖则不会。
`DependencySet::injects` 返回这些。

## `Requirement`

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Requirement { Required, Optional }
```

决定缺失的 provider 是否让 fiber 保持不激活。

## `DependencyResolution`, `ResolvedInject`, `CommittedView`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedInject {
    pub key: ProviderKey,
    pub provider: Option<FiberId>,
    pub requirement: Requirement,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DependencyResolution { /* entries: Vec<ResolvedInject> */ }
impl DependencyResolution {
    pub fn new(entries: Vec<ResolvedInject>) -> Self;
    pub fn entries(&self) -> &[ResolvedInject];
    pub fn is_ready(&self) -> bool;
    pub fn commit(&self) -> Result<CommittedView, CordisError>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CommittedView { /* entries: BTreeMap<ServiceId, ResolvedInject> */ }
impl CommittedView {
    pub fn lookup(&self, service: &ServiceId) -> Result<Option<FiberId>, CordisError>;
    pub fn entries(&self) -> impl ExactSizeIterator<Item = &ResolvedInject>;
}
```

- `DependencyResolution` 是 fiber 在其 context 中声明的依赖的当前解析。`is_ready()` 就是 `σ ⊨ d`
  （满意度，定义 21）：每个条目都有 provider 或是 optional。
- `commit()` 把 ready 解析**冻结**成一个 `CommittedView` —— 即 fiber 实际将据以运行的 `ω_n`。
  它在整个 load epoch 内不可变。
- `CommittedView::lookup` 返回 provider（`Option<FiberId>`）；如果该 fiber 从未声明过这个
  service，则返回 `Err(UndeclaredDependency)`（§6.3 的能力检查：组件只能访问它声明过的东西）。

**错误** —— 当必需的 provider 缺失时，`commit` 返回 `CordisError::InactiveDependency { key }`。
对于未知的 service，`lookup` 返回 `CordisError::UndeclaredDependency { service }`。

## `ProviderKey` 消费 vs `RegistryChange`

`RegistryChange` 位于 [supervisor](supervisor.zh.md)；它是 provide/withdraw 的结果。

## Payload codec

```rust
pub fn encode_service_payload<T: Serialize>(value: &T) -> Result<Vec<u8>, CordisError>;
pub fn decode_service_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, CordisError>;
```

用规范 `MessagePack` codec 编码/解码一个 service payload。native 与 Wasm dispatch 都使用它们，
这正是业务服务协议是 Kernel WIT 之上的 MessagePack 而非 TS 对象的原因（README："不承诺与 TS 插件
二进制兼容"）。

**错误** —— `encode` 返回 `CordisError::ServiceEncodeFailed { message }`；`decode` 返回
`CordisError::ServiceDecodeFailed { message }`。

## 示例

```rust
let dispatcher = Arc::new(CounterDispatcher::new(Arc::new(AtomicCounter::default())));
let client = CounterClient::new(dispatcher)?;   // checks name + ABI hash
let value = client.add(3).await.map_err(|e| match e {
    ServiceCallError::Transport(e) => e,
    ServiceCallError::Service(msg) => /* the provider's String */ ...,
})?;
```

或者，完全不做序列化，`CounterClient::from_native(arc)` 直接透过宏生成的 object-safe adapter
调用。
