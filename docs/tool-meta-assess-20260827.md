# 工具元评估：变体泛化 + 语言迁移评估器（dsh-tools v1.3.1）

> 日期：2026-08-27 · 回答用户四问的落地

## Q1: agent-msg 变体适合 Rust 化吗？
✅ 已 Rust 化（v1.3.0）。判断标准：
- 纯逻辑 + 无外部 SDK 依赖 → Rust（agent-msg 是 HTTP+JSON+黑板 KV）
- 官方 SDK 锁定（MCP/企微）→ 保留原语言
- 并发密集编排 → 可考虑 Go

## Q2: Rust/Go 化过程可工具化？
✅ 实现 `assess --tool X --src <path>`（语言迁移评估器）：
- 依赖扫描（SDK 锁定判断）
- 复杂度评估（行数分级）
- 决策输出（推荐语言 + 难度 + 收益）
实测：external-mcp→保留（SDK）、learning-sub→Rust（SSE）

## Q3: 变体/泛化过程可工具化+自动评估？
✅ 实现 `assess --bottleneck <描述>`（工具泛化评估器）：
- 泛化维度检测（跨设备/多租户/批量/异步/订阅）
- 输出建议 + 参考案例
实测：agent_thread 跨设备瓶颈 → 建议泛化（agent-msg 已实现）

## Q4: agent-msg 沿用最短沟通规则？
✅ agent-msg 已加 v2.3 门禁：>200字且非紧急 → 拒绝 + 提示短提醒模式
跨设备消息本来写黑板 → 天然适合「短提醒 + 内容写 data/」

## 完整命令（v1.3.1）
bb-read / bb-sub / handoff / workflow / bus-bridge / tailnet-proxy / agent-msg / agent-thread / assess

## 四问总结
| 问 | 答案 | 落地 |
|---|---|---|
| Q1 适合 Rust？ | ✅ | agent-msg/agent-thread Rust 化 |
| Q2 迁移可工具化？ | ✅ | assess 语言评估器 |
| Q3 泛化可工具化？ | ✅ | assess 泛化评估器 |
| Q4 沿用最短规则？ | ✅ | agent-msg v2.3 门禁 |
