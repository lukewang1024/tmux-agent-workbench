# Hook 修复对比用例

本目录保存 7 个故障场景、3 个正常流程对照。
会话、线程、PID、窗格和路径均使用测试数据，不读取真实 tmux 会话、对话或 Hook 配置。
检测用例复用了仓库已有的 `tests/golden/snapshot-v1.json`。

## 运行

在仓库根目录执行（需要 Unix、Python 3.12+、Rust/Cargo）：

```sh
cargo fetch --locked
python3 tests/compare-hook-recovery.py --baseline 95cda17 --repeats 10
```

脚本以指定提交为基线，只将 `HOOK_FILES` 列出的 8 个文件相对基线的当前差异应用到修复后快照。
两边注入完全相同的 Rust 用例，独立编译并执行；编译失败或缺失用例不会计作普通断言失败。
这些差分用例由脚本注入，不会自动进入普通 `cargo test`。
默认使用离线 Cargo 构建，所以首次运行前需下载锁定的依赖。

结果默认写入 `$XDG_STATE_HOME/tmux-agent-workbench/hook-comparison/`，
未设置时使用 `~/.local/state`；编译缓存使用 XDG cache 目录。
每次运行保存补丁、夹具、脚本、源码快照、构建日志和逐轮日志；生成报告包含原样复跑命令。
运行时输出可能包含本机绝对路径，分享前需另行检查，不应将整个输出目录直接提交。

## 适用边界

这是所选确定性回归用例的通过率，不是线上总体成功率。
重复运行用于观察稳定性，不代表独立随机样本或统计置信区间。
延迟确认场景只验证原事件进入重试队列；未覆盖完整重放投递、完整守护进程替换、共享后端拓扑和持久化重放保护。
