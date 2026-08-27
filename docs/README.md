# dsh-tools — 黑板固化工具集（Rust 版）

> 版本：v1.0.0 · 2026-08-27 · 从 Python 工具 Rust 化（用户要求「固化工具优先用 Rust」）

## 子命令

| 子命令 | 功能 | 替代 Python |
|---|---|---|
| `bb-read` | 看黑板摘要（本地模型 qwen2.5:3b，零 API 成本） | bb-read.py |
| `bb-sub` | 黑板事件桥常驻订阅器（SSE 长连接→inbox jsonl） | bb-sub-daemon.py |
| `handoff` | 任务一键移交（打包→任务卡→通知→台账） | task-handoff.py |

## 用法

```bash
# 看黑板（摘要模式，最小上下文）
dsh-tools bb-read notes/mac-mini/recovery-todo
dsh-tools bb-read --prefix notes/mac-mini/ --last 5    # 读本地订阅（避免黑板全量 3.2MB）
dsh-tools bb-read --scan ~/.dsh/inbox/bb/hr.jsonl     # 本地 inbox 摘要
dsh-tools bb-read --full <key>                        # 全文（决策场景）

# 常驻订阅（launchd 托管）
dsh-tools bb-sub --agent hr --prefixes "notes/mac-mini/,data/qa/"

# 任务移交 i9
dsh-tools handoff --name 34图OCR --files "./images" --to i9 --type ocr --priority P2 --desc "34 张图片 OCR"
```

## 设计

- 纯 std::net + serde_json（与 node-bridge/rust-blackboard 同风格），零外部 HTTP 依赖
- 交叉编译零负担（cargo-zigbuild 三平台）
- 本地模型摘要走 ollama HTTP API（qwen2.5:3b，零 API 成本，~2s/条）
- 资源：438KB（macOS arm64）

## 部署

- launchd：7 角色订阅器（com.dsh.bb-sub.*）已指向 Rust 二进制
- 产物：dist/dsh-tools-{macos-arm64,win-x64,linux-x64}-v1.0.0

## 版本记录

- v1.0.0（2026-08-27）：初版。bb-read/bb-sub/handoff 三子命令，替代三个 Python 工具。
