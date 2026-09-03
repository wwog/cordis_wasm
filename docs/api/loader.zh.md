# Loader（声明式 Entry）

`cordis-loader` 是 Cordis 的声明式一侧。一个应用被描述为一棵 **Entry** 树——一个 `cordis.json`/YAML 文档（或一条 include 链），它指明要运行哪些 components、用什么 config、在何种 isolation 与 intercept 规则下运行。loader 解析这棵树，对照已注册的 schemas 校验 configs，并通过 keyed diff 将其与当前状态 **reconcile**，驱动一个真正负责挂载/停止/更新 components 的 driver。

这对应论文 §5.2.1 的声明式配置：一个 entry 记录 `id, url, isolate,
intercept, config, disabled`（定义 81），而 reconcile 由定理 80 保证可靠。

文件语法本身——你可以在 `cordis.json`/YAML 里写什么、`isolate` 与 `intercept` 如何表现、以及 `!expr` Rhai 动态配置——记录在 [config](config.zh.md)。

## `EntryId`

```rust
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntryId(String);
impl EntryId {
    pub fn new(value: impl Into<String>) -> Result<Self, LoaderError>;
    pub fn as_str(&self) -> &str;
}
```

一个稳定的、非空的 Entry 标识符。这是 reconcile 的**key**：一个 entry 的身份是它的 `id`，而不是它的位置。

**错误** —— 对空标识符，`new` 返回 `LoaderError::InvalidEntryId(value)`。

## `ComponentRef`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComponentRef {
    Builtin(String),
    File(String),
}
impl ComponentRef {
    pub fn parse(value: &str) -> Result<Self, LoaderError>;
}
```

entry 中的 component 引用。两种 scheme：

- `builtin:<name>` —— 一个与 `BuiltinRegistry` 注册的进程内 factory（见 [wasm-driver](wasm-driver.zh.md)）。
- `file:<path>` —— 磁盘上的一个 `.wasm` component，相对应用基准目录解析。

**错误** —— 对缺失/未知的 scheme，`parse` 返回 `LoaderError::InvalidComponentRef`。裸名称（无 scheme）会被拒绝；scheme 是必需的。

## `IsolationRule`

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IsolationRule {
    Local(bool),
    Global(String),
}
```

entry 的 `isolate` 映射中某个命名 service 的 isolation 配置。`Local(true)` 创建一个由该 entry 拥有的全新 local realm（覆盖任何继承来的 realm）；`Local(false)` 移除继承来的 realm（回退到默认）；`Global(label)` 加入一个由使用相同 `label` 的 entries 共享的 realm。

## `EntrySpec`

```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntrySpec {
    pub id: EntryId,
    #[serde(default)] pub component: String,
    #[serde(default)] pub config: Value,
    #[serde(default)] pub disabled: bool,
    #[serde(default)] pub group: bool,
    #[serde(default)] pub intercept: BTreeMap<String, Value>,
    #[serde(default)] pub isolate: BTreeMap<String, IsolationRule>,
    #[serde(default)] pub children: Vec<Self>,
}
```

声明式树中的一个 entry。`group: true` 使它成为结构容器（无 component，只有 `children`）。`disabled` 被子节点继承（`effective_disabled`）。`intercept` 是一个 service-name → JSON-value 映射，会被合并进此节点之下各 entry 的 service config。

构造函数：

```rust
impl EntrySpec {
    pub fn leaf(id: impl Into<String>, component: impl Into<String>) -> Result<Self, LoaderError>;
    pub fn group(id: impl Into<String>, children: Vec<Self>) -> Result<Self, LoaderError>;
}
```

**错误** —— `id` 为空时两者都返回错误。

## `ManagedRealm`

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManagedRealm {
    Local { owner: EntryId, service: String },
    Global { label: String, service: String },
}
```

在解析完 local/global isolation 规则之后，已解析 entry 为某个 service 使用的 realm。`Local` 把 realm 限定到拥有它的 entry；`Global` 把它限定到由选择同一 group 的 entries 共享的一个 label。

## `ResolvedEntry`

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedEntry {
    pub spec: EntrySpec,
    pub component: Option<ComponentRef>,
    pub parent: Option<EntryId>,
    pub depth: usize,
    pub effective_disabled: bool,
    pub realms: BTreeMap<String, ManagedRealm>,
    pub intercept: BTreeMap<String, Value>,
}
impl ResolvedEntry {
    pub fn is_active(&self) -> bool { !self.spec.group && !self.effective_disabled }
}
```

一个完全解析的 entry：它的 spec、解析后的 component 引用（group 则为 `None`）、parent id、depth、effective-disabled 标志，以及从祖先继承合并的 realms/intercepts。

## `EntryDriver`

```rust
pub type LoaderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, LoaderError>> + Send + 'a>>;

pub trait EntryDriver: Send + Sync + 'static {
    fn start<'a>(&'a self, entry: &'a ResolvedEntry) -> LoaderFuture<'a, ()>;
    fn update<'a>(&'a self, previous: &'a ResolvedEntry, next: &'a ResolvedEntry) -> LoaderFuture<'a, ()>;
    fn stop<'a>(&'a self, entry: &'a ResolvedEntry) -> LoaderFuture<'a, ()>;
}
```

原子地执行一个 Entry 操作。**返回错误的方法必须把 runtime 留在其调用前的状态** —— `EntryTree` 利用这一保证，在后面的操作失败时回滚先前成功的操作。实现：`WasmEntryDriver`（见 [wasm-driver](wasm-driver.zh.md)）与 `PreflightDriver`（内建于 `check_entries`）。

## `EntryTree<D>`

```rust
pub struct EntryTree<D> { /* driver: Arc<D>, roots, resolved, schemas */ }
impl<D: EntryDriver> EntryTree<D> {
    pub fn new(driver: Arc<D>) -> Self;
    pub fn entries(&self) -> &BTreeMap<EntryId, ResolvedEntry>;
    pub fn roots(&self) -> &[EntrySpec];
    pub fn register_schema(&mut self, component: impl Into<String>, schema: Value);

    pub async fn reconcile(&mut self, roots: Vec<EntrySpec>) -> Result<(), LoaderError>;
    pub async fn create(&mut self, parent: Option<&EntryId>, entry: EntrySpec) -> Result<(), LoaderError>;
    pub async fn update(&mut self, entry: EntrySpec) -> Result<(), LoaderError>;
    pub async fn self_update(&mut self, id: &EntryId, config: Value) -> Result<(), LoaderError>;
    pub async fn self_disable(&mut self, id: &EntryId) -> Result<(), LoaderError>;
    pub async fn move_entry(&mut self, id: &EntryId, parent: Option<&EntryId>, index: usize) -> Result<(), LoaderError>;
    pub async fn remove(&mut self, id: &EntryId) -> Result<(), LoaderError>;
}
```

持有当前 entry 树，并使其与目标 reconcile。

### `reconcile`（keyed diff）

对 driver 应用一个 keyed 树 diff。算法是 stop → update → start：

1. **Resolve（解析）** 目标树（`resolve_entries`），检查重复/空的 ids，然后在*任何 driver 调用之前*对照已注册的 schemas **validate configs（校验配置）**。
2. **Stop（停止）** 不再激活、或其 component 发生变化的 entries，按 depth **降序**（最深的优先，所以子节点先于父节点停止）。
3. **Update（更新）** 仍处于激活、component 未变但其他方面有变化的 entries，按 depth 升序。
4. **Start（启动）** 新的（或重新启用的）entries，按 depth 升序（父节点先于子节点）。

**错误** —— 返回校验或 driver 错误，并且**不发布新的树**。如果之后的操作失败，已成功应用的 driver 操作会按逆序回滚（`rollback_error`），因此一次失败的 reconcile 会把 runtime 留在其调用前的状态。测试：`failed_reconcile_rolls_back_applied_operations_in_reverse`。

### 其他操作

- `create(parent, entry)` —— 在根或某个 `group` 之下插入。`MissingEntry`、`ParentNotGroup`。
- `update(entry)` —— 替换一个已有 entry，保持其位置不变。`MissingEntry`。
- `self_update(id, config)` —— 应用源自 entry 自身的 config 更新。
- `self_disable(id)` —— 禁用一个 entry。
- `move_entry(id, parent, index)` —— 移动一个 entry 而不改变其稳定 id。若移动会产生环则为 `EntryCycle`，`ParentNotGroup`。
- `remove(id)` —— 移除一个 entry 及其后代。`MissingEntry`。
- `register_schema(component, schema)` —— 为某个 component 注册一个 JSON Schema，供 `validate_configs` 使用。

## `LoaderError`

```rust
#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum LoaderError {
    InvalidEntryId(String),
    DuplicateEntry(EntryId),
    MissingEntry(EntryId),
    ParentNotGroup(EntryId),
    EntryCycle(EntryId),
    InvalidComponentRef(String),
    InvalidSchema { component: String, message: String },
    InvalidConfig { entry: EntryId, path: String, message: String },
    Driver(String),
    Include(String),
}
```

## `IncludeDocument` 与配置 include

`include.rs` 添加 `@cordisjs/include` 风格的外部配置文件。include 文档从 JSON 或 YAML 加载，求值（YAML `!expr` 标签针对当前 `ctx` snapshot 运行受限的 Rhai 表达式），打补丁，并可选择原子地写回。

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IncludeFormat { Json, Yaml }
impl IncludeFormat {
    pub fn from_path(path: &Path) -> Result<Self, LoaderError>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Patch { pub target: Option<EntryId>, pub action: PatchAction }

#[derive(Clone, Debug, PartialEq)]
pub enum PatchAction {
    Merge(Value),
    Replace(EntrySpec),
    Remove,
    Insert { index: usize, entry: EntrySpec },
}

#[derive(Debug)]
pub struct IncludeDocument { /* path, format, readonly, entries */ }
impl IncludeDocument {
    pub fn load(path: impl Into<PathBuf>, patches: &[Patch], expression_context: &Value) -> Result<Self, LoaderError>;
    pub fn from_source(path, format, readonly, source, patches, expression_context) -> Result<Self, LoaderError>;
    pub fn entries(&self) -> &[EntrySpec];
    pub fn entries_mut(&mut self) -> &mut Vec<EntrySpec>;
    pub fn readonly(&self) -> bool;
    pub fn write_back(&self) -> Result<(), LoaderError>;
}
```

- `load` 读取文件，从扩展名推断格式，并对 patches 求值。
- YAML `!expr` 标签在**受限**的 Rhai 引擎中针对 `ctx` JSON snapshot 求值（`eval`、`import`、`export`、`fn`、`while`、`loop`、`for`、`try`、`throw` 被禁用；操作与大小预算被设上限）。
- `write_back` 原子地重写具体化后的 entry 数组（临时文件 + rename）；若文档为只读则失败（Unix 上未设置 `0o222` mode）。

**错误** —— `load`/`from_source`：文件系统、语法、表达式或 patch 错误。`write_back`：只读文档或失败的文件系统操作。

## `ExprEvaluator`

```rust
#[derive(Default)]
pub struct ExprEvaluator { /* engine: rhai::Engine */ }
impl ExprEvaluator {
    pub fn evaluate(&self, expression: &str, context: &Value) -> Result<Value, LoaderError>;
}
```

针对 JSON `ctx` snapshot 求值一个受限的 Rhai 表达式，返回一个 JSON 值。

**错误** —— 禁用语法、预算耗尽或非 JSON 结果。

## `cordis check` 的 Entry 格式

CLI 的 `load_entries` 读取一个 `cordis.json`/YAML 文档，其根节点要么是一个 entry 数组，要么是一个带 `entries` 数组的对象，然后把 entries 交给 `check_entries`/`reconcile`。`CORDIS_SHARED` 环境变量为 YAML includes 中的 `!expr` 提供 `ctx` 值（见 [cli](cli.zh.md)）。
