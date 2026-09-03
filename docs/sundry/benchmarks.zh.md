# 0.1.0 基准测试基线

发布策略不接受未经测量瓶颈就引入的针对优化的公开调节项。基线是一个无依赖、stable-Rust 的
基准可执行程序：

```bash
cargo bench -p cordis-core --bench lifecycle
```

2026-09-03 的参考运行，Apple Silicon macOS，Rust 1.98.0，优化的 bench profile：

| 场景 | 迭代次数 | 基线 |
|---|---:|---:|
| 在 32 个不可变 Context overlay 中解析一个 realm | 1,000,000 | 64 ns/op |
| 完成一次 Fiber load/unload 往返 | 250,000 | 17 ns/op |

这些数字是观测值，而非回归阈值：共享 CI runner 对纳秒级硬性门槛而言噪声过大。0.1.0 未从
该基线增加任何公开调节旋钮。后续变更必须在同一场景下重现瓶颈，在需要时添加有代表性的基准，
并在为速度改动公开 API 之前同时报告绝对成本与相对改进。
