## v1.10.0 (2026-08-29)
- **新增 restart-guard 重启强制门（R011）**: dsh-tools restart-guard <插件目录>...
  - CLD/DSH 重启前沙箱模拟：deploy-check 全量检测 + 重启专属 4 项（type:module 匹配/ESM 导入完整性/符号链接/模块加载实测）
  - 任一 FAIL → 返回 1 禁止重启（防止 2026-08-29 central-inbox 缺 type:module 加载即崩事故重演）
  - node 自动探测（PATH → /opt/homebrew/bin → /usr/local/bin），不依赖调用者 PATH
  - 实测：修复版 0 FAIL 放行 / 未修复版 5 FAIL 拦截
# Changelog

## [1.9.0] - 2026-08-29

### 新功能
- BLACKBOARD_TOKEN 环境变量支持（所有 bb_* 请求带 X-Blackboard-Token 头，P1-1c）

## [1.8.0] - 2026-08-28

### 新功能
- ledger 子命令：工具台账自动化（扫 git tag + 漂移检测）

## [1.7.0] - 2026-08-28

### 新功能
- repo sync/status 子命令（Gitee 双仓同步 + 状态检查）

## [1.6.0] - 2026-08-28

### 新功能
- 新增 repo 子命令族：GitHub/Gitee 双仓 setup/push（合并 repo-pipeline 核心，SSH deploy key 通道）

## [1.5.0] - 2026-08-28

### 新功能
- 新增 version 子命令：自动化版本管理（bump+CHANGELOG+commit+tag）
