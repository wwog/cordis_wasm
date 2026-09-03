# 应用配置（`config.{json,yaml}`）

CLI 与 loader 都接受一个**声明式配置文件**，它描述整个应用：运行哪些组件、用什么 config、在何种 isolation 与 intercept 规则下运行、以及它们如何嵌套。该文件是 JSON 或 YAML，其根必须是一个条目列表，或一个带 `entries` 键的对象。`cordis check`、`cordis run`、`cordis inspect` 都接受它：

```sh
cordis check   examples/wasm-app/cordis.json
cordis run     examples/wasm-app/cordis.json
cordis inspect examples/wasm-app/cordis.yaml
```

loader 一侧是 `IncludeDocument`（见 [loader](loader.zh.md)），它加载文件并物化为一个 `Vec<EntrySpec>`；随后的 reconcile 步骤驱动 entry tree。本页记录**文件语法本身**——每个条目允许的字段、根形状、include、patch、以及 YAML 的 `!expr` 动态配置。

## 根形状

文件必须是**对象数组**，或者是一个 `entries` 字段为该数组的对象（这让你可以携带额外的顶层元数据）：

```yaml
# 两种形式均可：
- id: consumer
  component: file:../target/wasm32-wasip2/debug/app.wasm

# 或者：
entries:
  - id: consumer
    component: file:../target/wasm32-wasip2/debug/app.wasm
```

其它任何形式都被拒绝：`LoaderError::Include("include root must be an entry array or contain `entries`")`。

## 条目字段

一个**条目**就是一个 `EntrySpec`。字段及其到 `camelCase` 的 serde 重命名：

```rust
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntrySpec {
    pub id: EntryId,
    #[serde(default)]
    pub component: String,
    #[serde(default)]
    pub config: Value,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub group: bool,
    #[serde(default)]
    pub intercept: BTreeMap<String, Value>,
    #[serde(default)]
    pub isolate: BTreeMap<String, IsolationRule>,
    #[serde(default)]
    pub children: Vec<Self>,
}
```

| 字段 | 类型 | 含义 |
|---|---|---|
| `id` | 字符串 | 本条目的稳定标识 —— **reconcile 的键**。必填、非空。对已有条目的编辑会因 `id` 不变而被识别，因此 loader 执行的是更新而非"移除加新增"。 |
| `component` | 字符串 | 组件引用，`builtin:<name>` 或 `file:<path>`（见下文 `ComponentRef`）。叶子条目必填，`group` 条目为空。 |
| `config` | 对象 | 插件配置值，传给其 `activate`。在加载前按组件声明的 JSON Schema（`config_schema`）校验。 |
| `disabled` | bool | 保留条目但跳过挂载。`true` 会卸载该插件及所有等待其服务的插件；`false` 重新挂载。 |
| `group` | bool | 使其成为嵌套 `children` 并作为一个整体加载/卸载的**结构化分组**。group 条目不携带 `component`。 |
| `intercept` | 对象 | 服务特定的配置拦截：`服务名 → 配置值`。对该条目之下加载的插件，会被合并进解析出的 config（祖先条目在前）。在读取时访问 —— 修改 `intercept` **不会**触发热重载。 |
| `isolate` | 对象 | 服务隔离规则：`服务名 → 规则`（见下文 `IsolationRule`）。给条目一个服务名自己的实例/作用域。 |
| `children` | 数组 | 对于 `group` 条目，为嵌套的子列表。 |

除 `id` 外每个字段都有 `#[serde(default)]`，所以一个条目可以只含 `id` + `component` 的叶子。空 `id` 或重复 `id`、group 上的 `component`、或同一棵树中重复的叶子 `id` 都是校验错误。

### `ComponentRef`

`component` 必须使用 scheme —— 裸路径会被拒绝：

- `builtin:<name>` —— 一个通过 `BuiltinRegistry` 注册的进程内工厂（见 [wasm-driver](wasm-driver.zh.md) 与 wasm host）。
- `file:<path>` —— 磁盘上的 `.wasm` 组件，相对于应用的基础目录（配置文件所在目录）解析。该路径相对此基础目录。

### `IsolationRule`

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IsolationRule {
    Local(bool),
    Global(String),
}
```

| YAML 形式 | YAML 值 | 含义 |
|---|---|---|
| `service: true` | `Local(true)` | 进入**本地** realm：该条目及其后代在以此条目为 owner 的 realm 中读写 `service`。 |
| `service: false` | `Local(false)` | 离开 `service` 的继承 realm（回退到最近的祖先 realm）。 |
| `service: <label>` | `Global(label)` | 进入以 `label` 为键的**全局共享** realm；两个拥有相同 label 与 service 的条目共享一个 realm。 |

Realm 作用域分两层（`k ↦ ρ(k) ↦ σ(ρ(k))`）：entry tree 决定每个服务解析到哪个 **realm**，supervisor 决定哪个 provider 填充该 realm。参考的 `examples/wasm-app/cordis.json` 正是这样让 provider 与 consumer 对 `example.counter` 看到**同一个** `example` realm：

```json
{
  "entries": [
    { "id": "consumer", "component": "file:../../target/wasm32-wasip2/debug/wasm_counter_consumer.wasm",
      "config": {}, "isolate": { "example.counter": "example" } },
    { "id": "provider", "component": "file:../../target/wasm32-wasip2/debug/wasm_counter_provider.wasm",
      "config": {}, "isolate": { "example.counter": "example" } }
  ]
}
```

这里两个条目都把 `example.counter` 钉在全局 realm `example`，于是 consumer 的 `inject` 解析到 provider 的 `provide`。

## `config` 值与 JSON Schema

`config` 是任意 JSON 值。在条目启动前，它按组件的 **config schema** 校验——即组件 descriptor 里的 `config_schema` JSON，按 JSON Schema Draft 2020-12 解析：

- loader（`EntryTree::validate_configs` 与 driver 的 `validate_config`）用 `jsonschema::draft202012` 编译它，并对着 `entry.config` 运行。
- 不匹配会使整个 reconcile 失败，报 `LoaderError::InvalidConfig`，并指明条目、失败的 JSON-Pointer 路径与消息。schema 本身无效则报 `InvalidSchema`。
- `examples/` 里的 guests 声明严格 schema `{"type":"object","additionalProperties":false}`，所以只有 `config: {}` 通过；未知键被拒绝。
- 在 Draft 2020-12 里 `true`/`false` 是合法 schema：`true` 接受任意值，`false` 什么都不接受。

所以 `config` 并非自由格式：插件接受什么受其 `config_schema` 约束，错误的值是预检/加载时的**响亮**失败，而非静默跳过。

## Include 与 patch

配置文件可以把其他文件拉进来并重塑。`IncludeDocument::load(path, patches, context)` 读取一个文件，并在物化条目之前应用一列 [`Patch`]。每个 patch 指名一个 `target` `EntryId` 与一个动作：

| 动作 | 行为 |
|---|---|
| `Merge(value)` | 把 `value` 深合并进目标条目的 JSON，再重新解析为 `EntrySpec`。合并后的 `id` 必须等于目标。 |
| `Replace(entry)` | 用 `entry` 替换目标条目；`entry.id` 必须等于目标。 |
| `Remove` | 移除目标条目（及其 children）。 |
| `Insert { index, entry }` | 把 `entry` 插入到目标条目的 `children`（若无目标则插入根列表）的 `index` 处。目标必须是 group。 |

`PatchAction::Merge` 使用递归深合并：对象值逐键合并，任何非对象值都覆盖。

## Rhai 动态配置（`!expr`）

**仅 YAML。** 在 YAML 配置中，某个值可以是带标签的标量 `!expr <expression>`，其结果会替换字面值。该表达式由一个**受限制**的 Rhai 引擎求值，作用于一个 `ctx` 快照——它以变量 `ctx` 的身份绑定到 scope 中。

```yaml
- id: app
  component: builtin:test
  config:
    port: !expr ctx.port + 1
```

当 `ctx = {"port": 40}` 时，`config.port` 变为 `41`（loader 测试 `yaml_expr_is_evaluated_recursively_in_restricted_scope` 恰好覆盖了这个例子）。

### `ctx` 从哪来

快照是 `CORDIS_SHARED` 环境变量的 JSON（见 [cli](cli.zh.md)）；若未设置则为 `null`。它是运行时对共享应用状态的视图，在**加载**时呈现，使 config 本身可以是它的一个函数。

### 限制

`ExprEvaluator` 引擎被刻意沙箱化：

- 设置了求值预算：`10_000` 次最大操作、最大表达式深度 `32`、最大字符串 `64 KiB`、最大数组/映射大小 `10_000`。
- 一组固定符号被**禁用**，因此常用逃生通道都不可用：`eval`、`import`、`export`、`fn`、`while`、`loop`、`for`、`try`、`throw`。
- 结果必须能往返成 JSON 值（`rhai::serde::from_dynamic`）；非 JSON 结果报错。
- 该标签在**任意深度**被递归求值：出现在 bool、序列或嵌套映射中的 `!expr` 都在原地求值。

```yaml
- id: "app"
  component: builtin:test
  config:
    replicas: !expr ctx.replicas * 2
    tags:
      - !expr ctx.environment        # 序列中的 `!expr` 也会被求值
- id: "other"
  component: builtin:test
  config: !expr ctx.maybe_null       # 甚至整个 `config` 值
```

只有 `!expr` 标签被识别；其他任何标签（`!danger`、任意的 `!foo`）都会被拒绝：`LoaderError::Include("unsupported YAML tag ...")`。`!expr` 的值如果不是标量字符串则被拒绝。而在 **JSON** 中根本没有标签语法——动态值不可用；要用 `!expr` 请用 YAML。

## 只读与原子写回

`IncludeDocument` 会记录源文件是否为只读（在 Unix 上，无 `0o222` 写位）。其 `write_back()` 会把 entry 数组物化并写回源文件——JSON 美化输出或 YAML——通过一个原子性的临时文件重命名完成，并拒绝只读文档：`LoaderError::Include("include ... is read-only")`。loader 用它在自更新与源自变更场景中工作，因此插件想改写的 config 必须是可写的。

## 失败模式

| 症状 | 原因 |
|---|---|
| `InvalidComponentRef` | `component` 没有 `builtin:`/`file:` scheme，或使用了裸路径。 |
| `InvalidConfig` | `config` 不满足组件的 `config_schema`（Draft 2020-12）。会指明条目、JSON-Pointer 路径与消息。 |
| `InvalidSchema` | 组件的 `config_schema` 不是合法的 JSON Schema 对象/布尔值。 |
| `Include: unsupported YAML tag` | 使用了 `!expr` 以外的标签，或 `!expr` 不是标量字符串。 |
| `!expr ... failed` | Rhai 表达式出错、触及求值预算、使用了被禁用的符号，或返回了非 JSON 值。 |
| `DuplicateEntry` / `InvalidEntryId` | 两个条目共用 `id`，或某 `id` 为空。 |
| `ParentNotGroup` | `isolate`/`intercept`/patch 把非 group 当作了父级，或把叶子当作了 group。 |

## 另见

- [loader](loader.zh.md) —— `EntrySpec`、`EntryId`、`EntryTree::reconcile`、loader 错误枚举。
- [wasm-driver](wasm-driver.zh.md) —— 加载后的 entry tree 如何挂载到 Wasmtime fiber 上。
- [cli](cli.zh.md) —— `cordis` 子命令与 `CORDIS_SHARED`。
- [macros](macros.zh.md) —— 组件的 `#[cordis::component(config = ...)]` 如何生成其 `config_schema`。
