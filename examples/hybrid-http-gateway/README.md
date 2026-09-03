# 动静结合的 HTTP 网关示例

这是 `cordis_wasm` 里**静态组件 + 动态组件串联**的最小示例：一个 native HTTP 网关
（静态，`builtin:` 注册）发出请求生命周期事件，一个 WebAssembly 插件（动态，
`file:` 挂载）监听这些事件并记录结构化日志。

```
native 组件                             wasm 插件
──────────────────────                  ──────────────────────
HttpGatewayFactory                    LogPlugin
 (builtin:http-gateway)                 (wasm_log_plugin.wasm)
  │                                       │
  │ provide: example.http                 │ register_listener:
  │                                       │   http.request.started  (id=1)
  │ activate → spawn 后台循环              │   http.request.finished (id=2)
  │   每 1s:                              │
  │   ├─ emit "request.started" ──────────┼──► handle_event → host::log
  │   ├─ 模拟处理 (5ms)                   │       "request started: GET /hello …"
  │   └─ emit "request.finished" ─────────┼──► handle_event → host::log
  │                                       │       "request finished: status=… bytes=… duration=…ms"
  └──────────────────────                └─────────────────────
```

## 这演示了什么

- **动静混合**：gateway 是进程内 native factory，插件是 `.wasm` 组件；两者在同一棵
  Supervisor fiber 树里挂载，共享同一份 kernel 路由和生命周期。
- **native 提供基础，动态插件做扩展**：gateway 拥有"真实"的 HTTP 引擎（此处为模拟），
  插件只做观测（日志），不触碰协议实现。
- **跨边界事件**：native 侧用 `InstanceHost::dispatch_event` 发出事件，宿主
  `RuntimeKernel` 按 `(event, listener_id)` 路由到插件 fiber 的 `handle_event`。
- **能力边界**：插件只声明事件订阅，不依赖任何服务；网关只提供服务不依赖任何东西。
  两者通过 WIT kernel 接口通信，payload 走 MessagePack。

## 运行

```bash
# 1. 构建 wasm 插件（需要 wasm32-wasip2 target）
rustup target add wasm32-wasip2
cargo run -p xtask -- build-guests

# 2. 运行宿主（跑 3 个请求周期后自动退出）
cargo run -p hybrid-http-gateway
```

预期输出（stderr）：

```text
[host] 2 fibers active; running for 3 request cycles…
[Info] [cordis.guest] [fiber=3] request started: GET /hello (1 headers)
[Info] [cordis.guest] [fiber=3] request finished: status=200 bytes=128 duration=7ms
…
[host] shutdown complete
```

## 文件

| 文件 | 角色 |
|---|---|
| `src/http_gateway.rs` | native 组件：`ComponentFactory`/`ComponentInstance`，自带后台请求循环 |
| `src/main.rs` | 宿主：注册 builtin、`reconcile`、接 ConsoleExporter、运行 |
| `../../wasm-log-plugin/src/lib.rs` | wasm 插件：`Guest` 实现，监听两个事件并 `host::log` |
| `../../Cargo.toml` | workspace 成员 + xtask 会额外构建 `wasm-log-plugin` |

## 关键约定（两边的 ABI 必须一致）

native 侧 `http_gateway.rs` 和 guest 侧 `wasm-log-plugin/src/lib.rs` 共享同一份线协议：

| 常量 | native | guest |
|---|---|---|
| 事件名 | `http.request.started` / `http.request.finished` | 相同 |
| ABI hash | `[0xA1;32]` / `[0xB2;32]` | 相同 |
| listener id | `1` / `2` | 相同 |

改变任一字段（改名、换 hash、改 payload 类型）都必须两边同步，否则路由/lookup 失败。
ABI hash 固定了 payload 契约：`RequestStarted { path, method, headers }`、
`RequestFinished { status, bytes, duration_ms }`。

## 注意：事件的时序

网关在 `activate` 里 spawn 后台循环，循环**先 sleep 250ms 再发第一个事件**。这是为了给
Supervisor 时间把插件激活并完成 `register_listener`。如果移除这个延迟而有插件尚未就绪，
`dispatch_event` 会以 `ComponentFailed`（listener 未注册）失败，网关把这个周期丢弃并打
一条 warn 日志后继续——不会崩溃，但你会看到第一个周期的日志缺失。这是 Cordis 里典型的
"事件单播 + 监听者先注册"模型（见 `docs/tutorial/07-events-and-views.zh.md`）。
