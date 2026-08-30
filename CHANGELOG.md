## [1.17.0] - 2026-08-30
### 新增
- **queue-condense**：堆积队列语义浓缩（去重→bge-m3 聚类→qwen 归纳→写黑板+通知会话，Rust 版字节级 chunked 解析）

## [1.16.0] - 2026-08-30
### 新增
- **queue-drain**：堆积风暴抽走（扫描/抽走/汇总，备份保留可恢复）

## [1.15.0] - 2026-08-30
### 新增
- **load-gate**：资源总控门（本机磁盘/内存 + 服务器 SSH 探活 → 绿黄红门禁）

## [1.14.0] - 2026-08-30
### 新增
- **channel-audit**：通道卫生巡检（定向消息误走广播检测）
- **noise report/scan**：R018 无关消息反馈链

## [1.13.1] - 2026-08-30
### 新增
- restart-gate --post-health（升级后自动 health-check 确认）

## [1.13.0] - 2026-08-30
### 新增
- **health** 子命令族（health-check/upgrade-status/pitfall query/add，吸收 i9 health-check 插件 Rust 化）

## v1.12.0 (2026-08-29)
- **restart-gate v3**: 新增阶段 0 隔离小样本验证（cld-shell-sandbox-test，不动生产）
  - 三阶段：0=隔离小样本（壳行为/模式对话框/KeepAlive）→ 1=静态检查 → 2=动态压测
  - 参数 --skip-sandbox 可选跳过
## v1.11.0 (2026-08-29)
- **新增 restart-gate 统一重启强制门（R011 v2）**: dsh-tools restart-gate
  - 阶段 1: restart-guard 静态检查（插件完整性）
  - 阶段 2: restart-stress-test 动态压测（boot 冒烟 N 轮，100% PASS 才过）
  - 任一 FAIL → exit 1 禁止重启；全 PASS → exit 0
  - 参数: --rounds/--hold/--skip-stress/--checks-dir
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
