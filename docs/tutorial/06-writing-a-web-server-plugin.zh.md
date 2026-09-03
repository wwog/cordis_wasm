# 6. 编写一个 web 服务器插件

这是主要的练习。你将编写一个*提供* web 服务的插件——一个打开监听器并处理请求的 guest 组件。这一章也是运行时真实约束暴露得最厉害的一章，所以动手写任何代码之前，第一件事就是把它们直白地讲清楚。

## 现实的约束

guest 组件运行在 Wasmtime store 内。它只能通过 WIT kernel 接口，以及 host 链接进来的 WASI 接口触达外部世界。两条硬性事实决定了所有设计：

**事实 1 —— host 提供的是 `wasi:sockets`，而不是 `wasi:http`。** `crates/cordis-wasm/src/runtime.rs` 里的 `build_linker` 调用 `wasmtime_wasi::p2::add_to_linker_async`，它链接了文件系统、时钟、随机数、stdio 和 **sockets** 接口。wasmtime-wasi 48.0.1 中的那个 `p2` 模块接上了 `wasi:sockets/network`、`tcp`、`udp`、`ip-name-lookup` 和 `instance-network`——但*没有*接 `wasi:http`。linker 里没有 HTTP handler。导入 `wasi:http` 的 guest 会*链接*失败，而不只是无法运行，因为没有 host 函数来支撑那个 import。

所以在这个仓库里，"web 服务"意味着：**用 `wasi:sockets` 打开一个 TCP socket，自己来讲 HTTP 协议**（或者把一个小型 HTTP/1.1 parser 移植进 guest）。你在第 5 章的 `capability_for_wasi_import` 里看到过的 `wasi:http/ → network` 映射，是在告诉*策略检查器*：导入 `wasi:http` 的 guest 应当被当作网络使用者来对待——但 linker 今天没有任何东西能支撑它。如果你想让 `wasi:http` 可用，host 必须新增一个 HTTP 实现；那是 host 侧的扩展，超出了当前 crate 交付的范围。

**事实 2 —— `network` 被拒绝两次，而 CLI 直接拒绝它。** 即便是一个只使用 `sockets` 的 guest，在做任何事之前也需要两重独立的"允许"都通过：

1. **Cordis 策略。** `ArtifactPolicy::default()` 的 `allowed_capabilities = {}`——什么都不允许。在 `capabilities` 里声明 `"network"` 的 guest 会被 `validate_descriptor` 拒绝，除非 host 策略加进 `Capability::new("network")`。而 CLI 总是构建 `ArtifactPolicy::default()`。所以今天的 `cargo run -p cordis-cli -- run ...` 无法向任何插件授予 network。
2. **WASI sockets 上下文。** 即便 Cordis 策略放行，wasmtime-wasi 的 `WasiSocketsCtx` 也会默认把 `tcp`、`udp` 和 `ip_name_lookup` 设为**禁用**（`AllowedNetworkUses` 派生 `Default`，全部为 false）。`WasiCapabilities::build`（capability.rs）里的 `WasiCtxBuilder` 从不调用 `allow_tcp(true)`、`allow_udp(true)`、`allow_ip_name_lookup(true)` 或 `inherit_network()`。所以在传输层，每一次 socket connect/bind 都会被以 `PermissionDenied` 拒绝。

因此边界是明确而诚实的：**当前随仓库交付的 host 无法运行具备网络能力的 guest。** guest 代码写得出来、也是正确的；host 侧需要三处新增才能让它跑起来——一个放行 `"network"` 的 `ArtifactPolicy`、一个启用相关 socket 用法的 `WasiCapabilities`/`WasiCtxBuilder`，以及（针对 `wasi:http`）一个被链接进来的 HTTP 实现。这一章把两侧都给你：你要写的 guest，以及你必须提供的 host 配置。

## (a) guest crate

在现有 examples 旁边新建一个 guest crate——比如 `examples/wasm-http-server/`，包含以下必要文件。

`Cargo.toml`——与 counter examples 结构相同，`cordis-guest` 是唯一依赖，且 `crate-type = ["cdylib"]`：

```toml
[package]
name = "wasm-http-server"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
cordis-guest = { path = "../../crates/cordis-guest", version = "0.1.0" }

[lints]
workspace = true
```

把 `"examples/wasm-http-server"` 加进根目录 `Cargo.toml` 的 workspace `members`。

接下来是插件本身。结构与第 2 章是同一个骨架，只有三处不同：descriptor 声明了一个**要提供的服务**和一项 **`"network"` capability**；`activate` 接好 web 服务；`call_service` 分发该服务的方法。

它提供的服务是一个 web endpoint。在本仓库中，"web 服务"是一个 `call_service` 方法——HTTP 请求被*编码为一次 service call 的 payload*，而响应就是编码后的 reply。这才是实际存在的机制。下面这个 illustrative 代码块展示了它的形态；无论你是在 guest 内部、在 socket 之上自己解析 HTTP，还是通过 `call_service` 把请求当作编码后的值来接收，服务的边界都一样。

> **illustrative（示意）** —— 这个代码块意在传达预期的形态。`cordis-guest` SDK 不附带 HTTP client 或 socket 辅助工具；协议处理由你提供。`// REAL` 注释之外的内容，都是 guest SDK 与 kernel 实际提供的 API。

```rust
use cordis_guest::host::{
    self, CallContext, EventId, EventMode, EventReply, KernelError, ServiceId,
};
use cordis_guest::plugin::{Guest, PluginDescriptor};
use std::cell::RefCell;

const WEB_ABI: [u8; 32] = [0x57; 32];       // REAL: you choose/host agrees the hash
const SERVE_METHOD: u32 = 1;                // REAL: method id — see chapter 3
const LISTENER_ID: u64 = 7;

thread_local! {
    static REGISTRATION: RefCell<Option<host::Registration>> = const { RefCell::new(None) };
    static PORT: RefCell<u16> = const { RefCell::new(8080) };
}

struct HttpServer;

#[derive(serde::Serialize, serde::Deserialize)]   // REAL: serde is available via cordis-guest
struct ServeRequest {
    path: String,
    method: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct ServeResponse {
    body: String,
    status: u16,
}

fn web_service() -> ServiceId {
    ServiceId {
        name: "example.web".into(),
        abi_hash: WEB_ABI.to_vec(),
    }
}

impl Guest for HttpServer {
    fn descriptor() -> PluginDescriptor {
        PluginDescriptor {
            name: "example.wasm-http-server".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            wit_version: cordis_guest::KERNEL_ABI.into(),
            inject: Vec::new(),
            provide: vec![web_service()],
            config_schema: /* see (b) */ br#"{...}"#.to_vec(),
            capabilities: vec!["network".into()],     // REAL: must declare network
        }
    }

    fn activate(context: CallContext, config: Vec<u8>) -> Result<(), KernelError> {
        // REAL: parse config bytes (e.g. the port) with serde_json here.
        let registration = host::provide_service(context, &web_service())?;
        REGISTRATION.with(|slot| *slot.borrow_mut() = Some(registration));
        // ILLUSTRATIVE: open a TCP listener over wasi:sockets, or register the service so the
        // host routes requests to it. See the note below — this is where you supply a real socket
        // accept loop (wasi:sockets/tcp.tcp-socket, start-listen, accept) or a host-provided
        // HTTP dispatch. Nothing in cordis-guest-rs does this for you.
        Ok(())
    }

    fn deactivate(_context: CallContext) -> Result<(), KernelError> {
        REGISTRATION.with(|slot| slot.borrow_mut().take());
        Ok(())
    }

    fn call_service(
        _context: CallContext,
        service: ServiceId,
        method: u32,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, KernelError> {
        if service.name != web_service().name || method != SERVE_METHOD {
            return Err(KernelError::InvalidArgument("unknown web service method".into()));
        }
        let request: ServeRequest = cordis_guest::decode(&payload)?;
        // ILLUSTRATIVE: route `request.path`/`request.method` and build a response.
        let response = ServeResponse {
            status: 200,
            body: format!("hello over {}", request.path),
        };
        cordis_guest::encode(&response)
    }

    fn handle_event(
        _context: CallContext,
        _event: EventId,
        _listener_id: u64,
        _mode: EventMode,
        payload: Vec<u8>,
        _next_token: Option<u64>,
    ) -> Result<EventReply, KernelError> {
        Ok(EventReply::ContinueValue(payload))
    }
}

cordis_guest::export_plugin!(HttpServer);
```

真正真实的部分有：descriptor 字段、`host::provide_service` 与 `Registration` thread-local、`call_service` 里对 name+method 的匹配、`encode`/`decode` 往返，以及 `export_plugin!`。真正**由你提供**的部分有：计算/商定 `WEB_ABI` 与 `SERVE_METHOD`；解析 config bytes；socket accept loop 或 HTTP dispatch；以及任何 HTTP 解析。这不是你填错而留下的缺口——它就是 guest SDK 的当前状态，而 counter provider 的存在本身就证明这才是预期的形态。

### HTTP 侧哪里还只是骨架

自己读一遍 `crates/cordis-wasm/src/runtime.rs` 和 `capability.rs`，就能看清这条边界。三条 grep 就能确定它：

- `capability_for_wasi_import` 对 `wasi:sockets/` 与 `wasi:http/` 都返回 `Some("network")`。所以*策略层*把 HTTP 当作 network。
- `build_linker` 只调用 `wasmtime_wasi::p2::add_to_linker_async`。那会加进 sockets，但 `p2` 模块没有 `bindings::http::...::add_to_linker`。因此 `wasi:http` 无法被满足。
- `WasiCapabilities::build` 构建一个 `WasiCtxBuilder`，对每个 preopen 调用 `preopened_dir`，别的什么都不做。它从不启用 TCP/UDP/name-lookup，也不调用 `inherit_network`，所以 sockets 在传输层就被拒绝。

于是，对一个 web 服务器而言，"host 目前支持什么"的诚实总结如下：

| 层 | `wasi:sockets` (TCP) | `wasi:http` |
|---|---|---|
| Capability 检查 | 映射到 `network` | 映射到 `network` |
| Linker 是否提供 | **是** | **否** |
| 传输用法默认是否启用 | **否**（allow_tcp 关闭） | n/a |
| Cordis 默认策略是否放行 | **否** | **否** |

## (b) 接受 port/root 的 `config_schema`

我们的 web 服务器需要的 config 是一个 `port`，也许再加一个 `root` 目录。一份匹配的 schema：

```json
{
  "type": "object",
  "properties": {
    "port": { "type": "integer", "minimum": 1, "maximum": 65535 },
    "root": { "type": "string" }
  },
  "additionalProperties": false,
  "required": ["port"]
}
```

作为 descriptor 里的 bytes：

```rust
config_schema: br#"
{
  "type": "object",
  "properties": {
    "port": { "type": "integer", "minimum": 1, "maximum": 65535 },
    "root": { "type": "string" }
  },
  "additionalProperties": false,
  "required": ["port"]
}"#.to_vec(),
```

`port` 是**必填**的；`root` 是**可选**的，但一旦出现就必须是字符串。`additionalProperties: false` 会拒绝任何你没声明的 key。如果某个 entry 提供了 `{"port": 8080, "root": "./public"}`，host 会在激活之前对照这份 schema 校验它，然后把 `{"port":8080,"root":"./public"}` 的 bytes 交给你的 `activate`。随后你的 guest 解析它们来设定端口。如果某个 entry 提供了 `{"por": 8080}`，该 entry 会以 `InvalidConfig` 错误无法启动——这个拼写错误在组件真正激活之前就被抓住了。

## (c) 把 entry 加入 `cordis.json`

声明式 config 与 counter example 相同，只多了一处——为 web 插件所提供服务准备的 `isolate` 映射；而（对将调用它的 consumer 来说）需要的也是同样的映射：

```json
{
  "entries": [
    {
      "id": "web-server",
      "component": "file:../../target/wasm32-wasip2/debug/wasm_http_server.wasm",
      "config": { "port": 8080, "root": "./public" },
      "isolate": { "example.web": "web" }
    }
  ]
}
```

如果你加入一个调用 `example.web` 的 consumer，它也必须像 counter consumer 那样携带 `"isolate": { "example.web": "web" }`——否则它会在默认 realm 中解析该服务，永远看不到提供者。这里的 `config` 非空，这正是 (b) 中 schema 不是那种严格的空对象 schema 的原因：这个插件确实是想要一个端口。

## (d) 构建组件

针对 WASIp2 target 构建 guest：

```sh
cargo build --target wasm32-wasip2 -p wasm-http-server
```

这会产出 `target/wasm32-wasip2/debug/wasm_http_server.wasm`。因为该 crate 在 workspace 里，`cargo build -p wasm-http-server` 能解析到它，而 `cdylib` crate-type 会给你一个 component。注意：如果你把 crate 加进了 workspace、却没加进 `xtask`，那就跳过了 `build-guests` 这个便利，直接像上面那样构建即可。（要是你希望 `build-guests` 也把它带上，就把 crate 加进 `xtask` 的 package 列表。）

必须先安装 `wasm32-wasip2` target：

```sh
rustup target add wasm32-wasip2
```

位于 `target/wasm32-wasip2/debug/` 的 artifact，正是 (c) 中 `file:` 路径所引用的对象。

## (e) 运行 `cordis check` / `cordis run`

使用当前的 CLI，你就会在这里撞墙，而且值得精确知道这堵墙是什么。

```sh
cargo run -p cordis-cli -- check examples/wasm-app/cordis.json
```

对于一个在 `capabilities` 中声明了 `"network"` 的 guest，`check` 会这样失败：

```
cordis: driver failed: component capability `network` is denied
```

那是 `WasmHostError::CapabilityDenied` 经由 preflight driver 浮现出来。这不是你 guest 的 bug——是（CLI 所用的）`ArtifactPolicy::default()` 拒绝了该 capability。要让 `check` 通过，你必须使用一个 `allowed_capabilities` 包含 `"network"` 的 `ArtifactPolicy`，也就是说需要二者之一：

- 一个 **embedding host**，直接构建应用（不经由 CLI），构造一个带 `Capability::new("network")` 的 `ArtifactPolicy` 和一个启用了所需 socket 用法的 `WasiCapabilities`；或
- 一个 **对 CLI 的扩展**，从 config 或某个 flag 透传一个策略。

这两者在今天的仓库里都不存在。没有任何 CLI flag 可以放行某项 capability。所以如实的说法是：**你可以编写并构建这个 guest；但在 host 授予 `network` 之前，你无法用随附的 `cordis` CLI 运行它。** 这是当前实现的一个真实限制，也是第 5 章花一整页讲 `ArtifactPolicy` 的全部原因。

如果你确实构建了一个授予它的 host，host 端代码大致是这样：

```rust
// ILLUSTRATIVE — host side you must supply. Not in the CLI.
use cordis_wasm::{ArtifactPolicy, WasiCapabilities, WasmApplication};
use cordis_core::Capability;
use std::collections::BTreeSet;

let policy = ArtifactPolicy {
    kernel_abi: "0.1".into(),
    allowed_capabilities: BTreeSet::from([Capability::new("network")]),
    wasi: {
        // Wasmtime's WasiCtxBuilder must also enable TCP for sockets to actually work.
        // See WasiCapabilities::build in crates/cordis-wasm/src/capability.rs.
        WasiCapabilities::deny_all()
    },
};
```

注意，`wasi` 这一项正是你还需要教会 `WasiCapabilities`（或替换它所包裹的 `WasiCtxBuilder` 调用）调用 `allow_tcp(true)` 和 `inherit_network()` 的地方——否则，即便策略层放行了 `network`，socket 操作仍会返回 `PermissionDenied`。这就是事实 2 里的第二层拒绝，很容易漏掉。

假设策略授予了它、sockets 也已启用，那么 `run` 会照常打印 `running N fibers across M artifacts; press Ctrl-C to stop`，在你按下 Ctrl-C 之前一直提供服务，web fiber 处于 `Active` 状态。

## 什么是真的、什么由你提供——清单

| 方面 | 仓库中真实存在 | 由你提供 |
|---|---|---|
| guest `Guest` trait、descriptor、`export_plugin!` | 是 | 填入内容 |
| `provide_service` / `call_service` 接线 | 是 | 匹配 name + method id |
| `encode` / `decode` MessagePack 边界 | 是 | 你的 request/response 类型 |
| `config_schema` + host 校验 | 是 | schema JSON |
| `"network"` capability 闸门 | 是 | 声明它；再让 host 放行 |
| TCP listener accept loop | **否** —— 仍是骨架 | 基于 `wasi:sockets` 实现 |
| HTTP/1.1 解析 | **否** —— 仍是骨架 | 实现或链接一个 parser |
| `wasi:http` handler | **否** | host 侧扩展 |
| 放行 `network` 的策略 | **否**（CLI 默认拒绝） | host 侧配置 |

这里的通关密码是诚实：有趣且能工作的部分是 kernel 服务边界；web 服务器那部分正是 guest SDK 止步、由你接管的地方。那就是仓库的当前状态，值得明说，而不是遮掩过去。

## 试着把它弄坏

- 从 `capabilities` 中移除 `"network"`，但保留 guest 里的 socket import。现在 `check` 给出的是另一个错误——来自 `validate_wasi_imports` 的 `WASI import ... requires undeclared capability network`，而不是 `CapabilityDenied`。import 检查与声明检查彼此独立，而且两者都必须通过。
- 把 `config` 改成 `{"port": "8080"}`（字符串）。host 会在 schema 路径 `port` 处以 `InvalidConfig` 拒绝它——因为 `port` 被声明为 `type: integer`。
- 移除 `additionalProperties: false` 并加一个乱写的 key。现在它会静默通过——那份严格正是护栏。

下一章：[Events and views](07-events-and-views.zh.md)——一个对事件作出反应、并把结果呈现到可见之处的插件。
