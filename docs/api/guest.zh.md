# Guest SDK（`cordis-guest`）

WebAssembly 插件作者依赖的就是 `cordis-guest`。它提供生成的 kernel 绑定、若干面向 MessagePack 边界的类型化辅助函数，以及插件所实现的 `Guest` trait。插件会被编译成导出 `cordis:kernel@0.1.0` 的 `plugin` 接口的 Wasmtime Component；host 则导入 `host` 接口。

guest **按假设是不可信的**：host 的 effect 表才是最终权威。guest 即使丢弃 `Registration` handle 而不调用 `drop`，也仍会被 host 的 `force_cleanup` 清理，因为 host 持有 `EffectGuard`（见 [wasm](wasm.zh.md)）。

## `bindings`

```rust
pub mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "cordis-plugin",
        pub_export_macro: true,
    });
}
```

生成的 Cordis kernel 导入与插件导出。`pub_export_macro: true` 会生成 `export_plugin!` 所使用的 `export!` 宏。具体的 Rust 条目来自 `crates/cordis-guest/wit/kernel.wit` 中的 WIT（完整 WIT 见 [wasm](wasm.zh.md)）。

## `host` / `plugin` 再导出

```rust
pub use bindings::cordis::kernel::host;
pub use bindings::exports::cordis::kernel::plugin;
```

- `host` —— guest 用来触达 host 的导入接口：`host::call_service`、`host::provide_service`、`host::register_listener`、`host::dispatch_event`、`host::log`，以及类型 `CallContext`、`ServiceId`、`EventId`、`EventMode`、`EventReply`、`KernelError`、`Registration`。
- `plugin` —— guest 实现的导出接口：`plugin::Guest`、`plugin::PluginDescriptor`。

## `KERNEL_ABI`

```rust
pub const KERNEL_ABI: &str = "0.1";
```

本 SDK 版本实现的 kernel ABI。插件在其 descriptor 的 `wit_version` 中上报它；host 用 `ArtifactPolicy::kernel_abi` 对照校验。

## `encode` / `decode`

```rust
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, host::KernelError>;
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, host::KernelError>;
```

为动态 kernel 边界编码 / 解码一个类型化的 service/event 值，使用 `rmp-serde`（MessagePack）——与 host 的 `encode_service_payload` / `encode_event_payload` 相同的规范 codec，因此原生组件与 Wasm 组件采用同一种线上格式（wire format）。

**错误** —— 当序列化失败 / payload 与 `T` 不匹配时，返回 `invalid-argument`。

## `schema_json`

```rust
pub fn schema_json(value: &serde_json::Value) -> Result<Vec<u8>, host::KernelError>;
```

为其 descriptor 编码插件配置 schema（JSON 字节）。

**错误** —— 若 JSON 序列化失败，则返回 `invalid-argument`。

## `call_service`

```rust
pub fn call_service<Req, Res>(
    context: &host::CallContext,
    service: &host::ServiceId,
    method: u32,
    request: &Req,
) -> Result<Res, host::KernelError>
where
    Req: Serialize,
    Res: DeserializeOwned;
```

用类型化的 MessagePack 输入与输出调用 host 服务。编码 `request`，调用 `host::call_service(*context, service, method, &payload)`，并把应答解码为 `Res`。

**错误** —— host 错误或请求 / 应答的 codec 错误。

## `export_plugin!`

```rust
#[macro_export]
macro_rules! export_plugin {
    ($component:ident) => {
        $crate::bindings::export!($component with_types_in $crate::bindings);
    };
}
```

导出一个实现所生成的 plugin `Guest` trait 的类型。把它放在 crate 根部的底部：`cordis_guest::export_plugin!(CounterProvider);`。

## `Guest` trait

WIT 的 `plugin` 接口会生成一个由你实现的 `Guest` trait（位于 `cordis_guest::plugin`）。它的方法与 WIT 的 `plugin` 函数一一对应：

```rust
pub trait Guest {
    fn descriptor() -> PluginDescriptor;
    fn activate(context: CallContext, config: Vec<u8>) -> Result<(), KernelError>;
    fn deactivate(context: CallContext) -> Result<(), KernelError>;
    fn call_service(
        context: CallContext, service: ServiceId, method: u32, payload: Vec<u8>,
    ) -> Result<Vec<u8>, KernelError>;
    fn handle_event(
        context: CallContext, event: EventId, listener_id: u64, mode: EventMode, payload: Vec<u8>,
        next_token: Option<u64>,
    ) -> Result<EventReply, KernelError>;
}
```

- `descriptor` 返回插件的静态 descriptor：`name`、`version`、`wit_version`（`KERNEL_ABI`）、`inject`/`provide` 的 service id、`config_schema`（JSON 字节）、`capabilities`。
- `activate` 在 fiber 加载时被调用一次，参数为 context 与 config payload。它可以调用 `host::provide_service` / `host::register_listener` 向 host 注册。
- `deactivate` 在卸载时被调用；随后 host 会强制清理任何剩余的注册。
- `call_service` 把一次服务调用分发给插件。
- `handle_event` 分发一次事件回调；返回 `EventReply::ContinueValue` / `EventReply::BreakValue`。

## 示例：`wasm-counter-provider`

取自 `examples/wasm-counter-provider/src/lib.rs`：

```rust
use cordis_guest::host::{
    self, CallContext, EventId, EventMode, EventReply, KernelError, ServiceId,
};
use cordis_guest::plugin::{Guest, PluginDescriptor};
use std::cell::RefCell;

const COUNTER_ABI: [u8; 32] = [0x43; 32];
const GET_METHOD: u32 = 1;

thread_local! {
    static REGISTRATION: RefCell<Option<host::Registration>> = const { RefCell::new(None) };
    static VALUE: RefCell<u64> = const { RefCell::new(0) };
}

struct CounterProvider;

impl Guest for CounterProvider {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor {
            name: "example.wasm-counter-provider".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            wit_version: cordis_guest::KERNEL_ABI.into(),
            inject: Vec::new(),
            provide: vec![counter_service()],
            config_schema: br#"{"type":"object","additionalProperties":false}"#.to_vec(),
            capabilities: Vec::new(),
        }
    }

    fn activate(context: CallContext, _config: Vec<u8>) -> Result<(), KernelError> {
        let registration = host::provide_service(context, &counter_service())?;
        REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
        Ok(())
    }

    fn deactivate(_context: CallContext) -> Result<(), KernelError> {
        REGISTRATION.with(|slot| slot.borrow_mut().take());
        Ok(())
    }

    fn call_service(
        _context: CallContext, service: ServiceId, method: u32, payload: Vec<u8>,
    ) -> Result<Vec<u8>, KernelError> {
        if service.name != counter_service().name || method != GET_METHOD {
            return Err(KernelError::InvalidArgument("unknown service method".into()));
        }
        let increment = if payload.is_empty() {
            1
        } else {
            cordis_guest::decode::<u64>(&payload)?
        };
        let value = VALUE.with(|value| {
            let mut value = value.borrow_mut();
            *value += increment;
            *value
        });
        cordis_guest::encode(&value)
    }

    fn handle_event(
        _context: CallContext, _event: EventId, _listener_id: u64, _mode: EventMode,
        payload: Vec<u8>, _next_token: Option<u64>,
    ) -> Result<EventReply, KernelError> {
        Ok(EventReply::ContinueValue(payload))
    }
}

fn counter_service() -> ServiceId {
    ServiceId { name: "example.counter".into(), abi_hash: COUNTER_ABI.to_vec() }
}

cordis_guest::export_plugin!(CounterProvider);
```

消费者（examples/wasm-counter-consumer）是其镜像：它声明 `inject: vec![counter_service()]`，并在 `activate` 中调用 `cordis_guest::call_service(&context, &counter_service(), GET_METHOD, &1_u64)`。注意 ABI 哈希 `[0x43; 32]` 在两侧都是硬编码的——guest 不运行过程宏，因此它必须按约定与 host 生成的哈希一致。

## 错误

`KernelError` 的变体（来自 WIT）：`invalid-argument`、`inactive-context`、`inactive-dependency`、`undeclared-dependency`、`capability-denied`、`internal`。当 codec 或 host 拒绝该操作时，`encode`/`decode`/`schema_json`/`call_service` 返回 `KernelError`。
