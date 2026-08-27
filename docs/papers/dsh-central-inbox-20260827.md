# dsh-central-inbox 实现方案（黑板→中枢上下文插入）

> 日期：2026-08-27 · 依据：群组记忆/黑板模式 + Cordis 事件系统 + Claude Channels 论文基础

## 目标
MBP/i9 写黑板 notes/mac-mini/* → **自动注入中枢会话上下文**（等价 agent_send），
中枢下一轮天然看到，无需主动查。

## 技术基础（已沉淀）
1. **群组记忆黑板模式**：agent 不直接对话，读写共享黑板；观察者看到匹配输入即行动
2. **Cordis 事件系统**：ctx.on 监听（effect 自动清理）+ emit/parallel/serial/waterfall
3. **Claude Code Channels**：MCP notification 注入运行中会话上下文（业界验证）

## 架构

```
MBP/i9 写黑板 notes/mac-mini/*
  → 黑板事件桥 8803 SSE（Rust 黑板原生）
  → dsh-central-inbox 插件（Cordis，宿主内）
     ├─ ctx.on 监听黑板事件桥新消息
     ├─ 过滤：notes/mac-mini/* + collab/*（to 中枢的）
     ├─ store.addMessage(thread, from=mbp/i9, to=fa1f9150, text='看黑板 <key>')
     └─ flushQueue() → 注入中枢会话队列 → 下一轮上下文自动看到
```

## 与现有机制关系
- central-wake.py（已上线）= 兜底（中枢激活后主动查触发标记）
- dsh-central-inbox 插件（本项目）= 根治（消息自动插入上下文）
- 两者并存：插件注入为主，wake 兜底

## 实现步骤
1. 基于 agent-bus 插件源码（已有 store.addMessage/flushQueue）
2. 新建 dsh-central-inbox 插件：监听黑板 8803 SSE
3. 过滤 → addMessage → flushQueue
4. 部署验证：MBP 发消息 → 中枢下一轮自动看到

## 论文基础（已入库 ops-science-research）
- 多智能体群组记忆-黑板模式生产实践（8 chunks）
- Cordis事件系统与ClaudeChannels上下文插入（10 chunks）
