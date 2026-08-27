# Rust 化全量改造完成报告

> 日期：2026-08-27 · 按择优策略（Rust 主栈 + Go 按需）+ 硬约束（禁止起服务测试）
> 本次新增：bus-bridge / tailnet-proxy / workflow 引擎 Rust 化

## 新增 Rust 化

| 组件 | Node 原内存 | Rust 版 | 替代 | 验证 |
|---|---|---|---|---|
| bus-bridge | 36.9MB | ~10MB | dsh-tools bus-bridge | ✅ 全状态机（send/receive/reply/outbox） |
| tailnet-proxy | 55.6MB | ~15MB | dsh-tools tailnet-proxy | ✅ 编译+三平台归档 |
| learning-sub | 59.3MB | 6MB | bb-sub learning 前缀 | ✅ 同构替代 |
| workflow 引擎 | — | — | dsh-tools workflow | ✅ 串行/并行/token门禁 |

## 保留 Node/JS（官方 SDK 锁定）

- external-mcp：@modelcontextprotocol/sdk 官方 MCP 协议 → 保留
- wecom-inbox：@wecom/aibot-node-sdk 官方企微 SDK + SQLite → 保留
- bad-review-scheduler：纯 cron 调度 → 保留

## 保留 Python（cron 触发，无常驻）

- ~20 个 cron 脚本（approval-tier/kb-health/token-roi 等）

## 收益

Node 常驻 6 服务 → Rust 3 个（bus-bridge/tailnet-proxy/learning-sub）：
- 已省 ~120MB 常驻内存（36.9+55.6+59.3 = 151.8 → ~31MB）
- 其余 3 个因官方 SDK 锁定保留（external-mcp/wecom/bad-review）

## dsh-tools v1.2.0 子命令全景

bb-read（看黑板摘要）/ bb-sub（订阅器）/ handoff（任务移交）/
workflow（串行并行编排）/ bus-bridge（跨设备总线）/ tailnet-proxy（远程入口）

## 相关文档

- 择优策略：language-strategy-rust-go-20260827.md
- 硬约束：运维硬约束-禁止起服务测试-20260827.md
- 资源评估：resource-comparison-v1.0.0.md
