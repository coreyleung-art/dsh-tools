# dsh-tools — 黑板固化工具集（Rust）

跨设备协作的 Rust 固化工具集：黑板读写、部署审查、版本管理、双仓流水线、台账自动化。

## 子命令

| 命令 | 用途 |
|------|------|
| `bb-read <key>` | 看黑板（摘要/全文）|
| `bb-sub --agent NAME --prefixes` | 黑板订阅 |
| `handoff --name X --files 'a b' --to i9` | 任务移交 |
| `workflow <json>` | 串行/并行编排（token 预算门禁）|
| `bus-bridge` | 跨设备总线桥 |
| `tailnet-proxy` | Tailscale 代理 |
| `deploy-check <插件目录>` | 跨设备部署安全审查（依赖/API 漂移/解析链）|
| `version --type fix/feat/breaking --desc` | **自动化版本管理**（bump+CHANGELOG+commit+tag）|
| `repo setup/push/sync/status` | **GitHub/Gitee 双仓全链路** |
| `ledger` | **工具台账自动化**（扫 git tag + 漂移检测）|

## 版本管理（version 子命令）

```bash
dsh-tools version --type fix --desc "修复 xxx" --repo <仓库路径>
# 自动：bump → CHANGELOG → git commit → git tag → 推送提示
```

## 双仓流水线（repo 子命令）

```bash
dsh-tools repo setup --repo <路径>          # 建仓（GitHub+Gitee）+ SSH + deploy key + 推送
dsh-tools repo push --repo <路径>           # 推送当前分支
dsh-tools repo sync --repo <路径>           # Gitee 双仓同步 + Actions 触发
dsh-tools repo status --repo <路径>         # 双仓状态 + ahead/behind + Actions
```

## 台账自动化（ledger 子命令）

```bash
dsh-tools ledger                 # 扫全部已知仓库的 git tag
dsh-tools ledger --json          # JSON 输出
dsh-tools ledger --repo <路径>   # 只扫指定仓库
```

## 三平台产物

- `dsh-tools-macos-arm64-v1.8.0`（GitHub Release）
- `dsh-tools-win-x64-v1.8.0.exe`（GitHub Release）
- `dsh-tools-linux-x64-v1.8.0`

## 版本历史

见 CHANGELOG.md（v1.4.3 deploy-check / v1.5.0 version / v1.6.0 repo / v1.7.0 sync+status / v1.8.0 ledger）

## 设计要点

- 纯 std + serde_json（+chrono），零外部命令依赖（调系统 git/gh/curl/openssl）
- 不绑定端口（硬约束）：只做静态分析与外部命令编排
- 推送兜底：GitHub 主站 443 被墙时用 SSH deploy key（`GIT_SSH_COMMAND`）
