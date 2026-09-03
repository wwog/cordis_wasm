# 0.1.0 依赖策略审查

审查日期：2026-09-03。

针对发布候选版 `Cargo.lock` 执行的命令：

```bash
cargo-deny 0.20.2 check bans licenses sources advisories --hide-inclusion-graph
cargo-audit 0.22.2 audit
```

结果：

- cargo-deny：advisories、bans、licenses 与 sources 全部通过。重复的传递版本
  仅是警告，因为 Wasmtime 48 与 wit-bindgen 0.61 有意地解析了若干 WebAssembly 与
  平台支持 crate 的不同版本。
- cargo-audit：没有漏洞导致失败。它报告了 `smartstring 1.0.1` 的信息性
  unmaintained advisory `RUSTSEC-2026-0249`。
- `smartstring` 是构建期/经 `rhai 1.26.0` 传递的依赖，不被 Cordis 公开 API 暴露，
  且在此次扫描中没有漏洞 advisory。Rhai 已被约束到受限的 Include 表达式求值器当前使用的
  1.26 系列。该发布接受这一信息性警告，并将在 Rhai 提供受维护路径时移除或替换该依赖；
  漏洞 advisory 仍然属于发布阻断项。
- 每个可发布的 path 依赖现在都带有 `version = "0.1.0"`；示例的 path 依赖同样如此，
  因此通配符依赖策略通过。
- 允许的来源仅限 crates.io；未知 registry 与 git 来源均被拒绝。

CI 在每次 push 与 pull request 时都会重复执行 cargo-deny 与 RustSec 检查。因此，即使源码树
未发生变化，一个新的 advisory 也可能阻断后续的发布。
