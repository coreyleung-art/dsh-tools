# Rust + Go 双语言择优选择策略（DSH 工具层）

> 版本：v1.0 · 2026-08-27 · 依据：纯客观语言对比（剔除迁移成本，Go 47 vs Rust 44.5）+ 已有 Rust 栈
> 决策：**Rust 主栈保持 + Go 按需引入（并发密集/快速迭代组件）**——分层语言策略

## 一、选型矩阵（按组件特征择语言）

| 组件特征 | 首选语言 | 理由 |
|---|---|---|
| 资源敏感的常驻服务（内存/CPU 关键） | **Rust** | 5.97MB 无 GC 确定性，长期运行成本低 |
| 安全暴露面（多设备可达的 HTTP/SSE 服务） | **Rust** | 编译期内存安全更硬（零 unsafe 可写服务） |
| 需极小体积分发（三平台拷贝即跑） | **Rust** | 428KB vs Go 2-8MB |
| 并发密集编排（大规模并行任务/工作流调度） | **Go** | goroutine 10万级，channel 天然 |
| 快速迭代的内部工具（改得勤、逻辑简单） | **Go** | 编译 ~1s，语法简单，无借用检查 |
| 标准库能覆盖的 HTTP 服务 | **Go** | net/http 完整，无需选型 |
| 复用现有 Rust 栈逻辑（黑板/桥/订阅） | **Rust** | 同栈改造成本最低 |

## 二、当前组件归属（已定）

| 组件 | 语言 | 状态 |
|---|---|---|
| node-bridge（传输层五线程） | Rust ✅ | v1.2.0 |
| rust-blackboard（KV+SSE） | Rust ✅ | v0.6.0 |
| rust-genebank（AI 网盘） | Rust ✅ | v1.0.0 |
| dsh-tools（bb-read/bb-sub/handoff/workflow/bus-bridge） | Rust ✅ | v1.1.0 |
| tailnet-proxy（远程入口） | Node → 待评估 | 55.6MB |
| external-mcp / wecom-inbox | Node → 待评估 | 49.8/53MB |
| workflow 引擎 | Rust ✅ | 已实现（std::thread+mpsc） |

## 三、双语言潜在冲突预判（提前防范）

### 冲突点 1：端口竞争
- **风险**：两个运行时（Rust 服务 + Go 服务）可能抢同一端口（如已有 3081 冲突教训）
- **防范**：端口分配表固定化（见第四节），新服务先登记端口再启动；launchd KeepAlive 服务间端口检查

### 冲突点 2：工具链管理
- **风险**：cargo/rustup + go 两套工具链，版本/路径混乱
- **防范**：统一 `~/dsh-collab/rust-toolchain/Makefile` 扩展 `go` target；manifest.json 增加 `language` 字段标注

### 冲突点 3：二进制发布管线
- **风险**：两套构建（zigbuild vs CGO_ENABLED=0）+ 两套产物命名
- **防范**：统一 `scripts/build.sh` 支持双语言；产物统一放 dist/ 按 `{name}-{os}-{arch}-{ver}` 命名

### 冲突点 4：协议一致性
- **风险**：Rust 服务与 Go 服务通过 HTTP/JSON 交互，schema 漂移
- **防范**：统一走黑板协议（KV + HTTP + JSON）；契约变更先改黑板文档再改代码；关键消息加 version

### 冲突点 5：运行时资源叠加
- **风险**：Rust 6MB + Go 15MB 各常驻多个，25GB 内存被蚕食
- **防范**：常驻服务总量控制（预算表）；Go 仅用于必要场景；优先 Rust（省内存）

### 冲突点 6：团队知识负担
- **风险**：双语言维护，智能体/人类都要会两种
- **防范**：选型矩阵作为硬规则；新组件先查矩阵定语言；文档标注语言

### 冲突点 7：启动顺序/依赖
- **风险**：Go 服务依赖 Rust 服务（或反之），launchd 启动顺序竞争
- **防范**：launchd 服务间用 `KeepAlive` + 健康检查（服务启动后 curl /health 确认依赖就绪）

### 冲突点 8：调试/日志格式
- **风险**：Rust 结构化日志 vs Go 默认日志，混合难排查
- **防范**：统一日志规范（logging-spec-v1.0.md），Go 侧按同格式输出

## 四、端口分配表（防冲突）

| 端口 | 服务 | 语言 |
|---|---|---|
| 8791 | bus-bridge | Rust |
| 8792 | 黑板 KV | Rust |
| 8797 | 黑板订阅状态 | Python（临时） |
| 8799 | 临时共享 HTTP | Python（临时） |
| 8801 | genebank 网盘 | Rust |
| 8803 | SSE 事件桥 | Rust |
| 3081 | tailnet-proxy 远程入口 | Node（待评估） |

**规则**：新增服务必须登记端口（写本表 + 黑板 data/registry/），禁止未登记占用。

## 五、决策流程（新组件选语言）

```
新组件需求
  ├─ 资源敏感常驻？→ Rust
  ├─ 安全暴露面？→ Rust
  ├─ 并发密集编排？→ Go
  ├─ 快速迭代内部工具？→ Go
  ├─ 复用现有 Rust 栈？→ Rust
  └─ 其他 → 默认 Rust（保持主栈一致）
```

## 六、版本记录

- v1.0（2026-08-27）：初版。基于纯客观对比（Go 47 vs Rust 44.5）+ 已有栈 + 8 个冲突预判 + 端口分配表。
