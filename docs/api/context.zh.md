# Context

`cordis_core::Context` 是服务 realm 与 intercept 层的一个廉价、不可变、可克隆的 **overlay**。它不是可变的容器：每个作用域操作都会返回一个*新的* context，其 parent 指回你调用它的那个 context，因此子 context 继承其祖先的映射，且从不修改父 context。它是 TS `ctx` 对象的 Rust 对应物，也是论文递归 context `Γ∞` 的实现——一条不可变的节点链，见 [semantics.zh.md](../semantics.zh.md) §3。

内部，`Context` 是一个 `Arc<ContextNode>`：

```rust
struct ContextNode {
    parent: Option<Arc<ContextNode>>,
    fiber: FiberId,
    realms: BTreeMap<ServiceId, RealmId>,
    intercepts: BTreeMap<ServiceId, Arc<Value>>,
}
```

## `Context::root(fiber)`

```rust
pub fn root(fiber: FiberId) -> Self
```

创建应用树的 root context，绑定到 `fiber`，且不带任何 realm 或 intercept。每个子 context 都通过 parent 链共享这一起源。

## `fiber`

```rust
pub fn fiber(&self) -> FiberId
```

返回该 context 所属的 `FiberId`。`root` 在构造时绑定它；`extend` 会重新绑定它。

## `extend`

```rust
#[must_use]
pub fn extend(&self, fiber: FiberId) -> Self
```

创建一个属于 `fiber` 的**空 overlay**，继承当前 context 的全部 realm 与 intercept。子 fiber 正是以此获得扩展其父 context 的 context：`mount_dynamic` 与方法级 inject 路径都会调用 `base.extend(child)`——子 context 共享父 context 的 resolver 视图，但归不同的 fiber 所有。父 context 不受影响。

## `isolate`

```rust
#[must_use]
pub fn isolate(&self, service: ServiceId, realm: RealmId) -> Self
```

创建一个恰好覆盖一个 `service` 的 realm 的 overlay，保持与父 context 相同的 fiber。在返回的 context 之下，对 `service` 的读写会按 `realm` 解析，而非父 context 的映射，因此可以提供不同的提供者而不影响父作用域。把同一个 `realm` 传给两次 `isolate` 调用（针对同一个 service）会合并它们的作用域——映射是按 `RealmId` 进行的，这正是 realm 能用作分组键的原因。

这是论文 `isolate` 的实现（定义 25）：一个全新的派生 context，丢弃即恢复，不带任何被跟踪的 effect。

## `intercept`

```rust
#[must_use]
pub fn intercept(&self, service: ServiceId, value: Value) -> Self
```

在一个新 overlay 中（与父 context 相同的 fiber）为 `service` 添加一个**动态 intercept 层**。`value` 是任意 JSON 值，`service` 的消费者会看到它被合并进该服务已解析的 config。父 context 不受影响；你的 `intercept` 条目只在读取时被查阅——修改它并不会重载 fiber。

## `resolve_realm`

```rust
pub fn resolve_realm(&self, service: &ServiceId) -> Result<RealmId, CordisError>
```

通过沿祖先链向外走、直到找到映射 `service` 的节点，为 `service` 解析**最近**的 realm 覆盖。

**错误** —— 当没有任何 overlay 定义该服务时，返回 `CordisError::MissingRealm { service }`。

这是 realm 解析的前半部分。完整 key 有两层：`ProviderKey::new(service, context.resolve_realm(service)?)`，因此 supervisor 的 provider 表以 `(service, realm)` 作为 key。

## `intercept_layers`

```rust
pub fn intercept_layers(&self, service: &ServiceId) -> Vec<Arc<Value>>
```

按从**最外层**到**最内层**的顺序返回 `service` 的 intercept `value`，即祖先优先。如果没有节点拦截 `service`，则 vector 为空。

## 行为说明

- **不可变 overlay。** 没有方法会修改已存在的节点。共享同一条 parent 链的两个 context，各自恰好看到自己创建的映射加上继承的映射。
- **可克隆。** `Context` 派生 `Clone` 与 `Debug`；克隆是一次 `Arc` 克隆，而非深拷贝。
- **所有权 vs 作用域。** `extend` 改变*所有权*（将观察该 context 的 fiber）；`isolate` 与 `intercept` 改变*作用域*（service 解析所处的 realm / config）而不改变 fiber。`mount_dynamic` 组合两者：先扩展至新的 fiber，再把每个声明（inject 或 provide）的服务 isolate 进该条目的 realm。

## 示例

```rust
let database = ServiceId::new("database", [0; 32]);
let root_realm = RealmId::next();
let local_realm = RealmId::next();

let root = Context::root(FiberId::next()).isolate(database.clone(), root_realm);
let local = root.isolate(database.clone(), local_realm);

assert_eq!(root.resolve_realm(&database), Ok(root_realm));
assert_eq!(local.resolve_realm(&database), Ok(local_realm));
```

## 错误

- `MissingRealm { service }` —— 没有任何 overlay 定义该服务。
