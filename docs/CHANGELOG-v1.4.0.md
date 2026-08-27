# dsh-tools v1.4.0 · CHANGELOG

## 新增：deploy-check — 跨设备插件部署安全审查（沙箱隔离模拟机制 v1.0）

**来源**：MBP 部署 agent-bus/central-inbox 实战产出（不模拟会崩溃：缺传递依赖 / definePlugin API 漂移 / 解析链不可达）
**请求**：MBP 提案「工具化 + 插件化/落链 + 采纳为泛化设备底层流程规则」→ 本版本实现工具化部分

### 功能

```
dsh-tools deploy-check <插件目录> [--runtime <CLD runtime>] [--json]
```

| 检查项 | 检测内容 | 级别 |
|--------|---------|------|
| 基础结构 | 插件目录缺 lib/ 或 package.json | FAIL |
| 依赖解析 | peerDependencies / dependencies 声明的包在宿主是否齐全 | FAIL（缺失）|
| 版本对齐 | 宿主包版本 vs 声明版本（major.minor 比较） | WARN（漂移）|
| API 漂移 | import { X } 的具名导出是否真的存在于宿主包（如 definePlugin 在 cordis 4.x 已移除） | FAIL |
| 解析链 | import 'pkg' 的包在宿主 node_modules 是否可达 | FAIL |

### 退出码

- `0`：全 OK —— 可安全部署（收包端仍需二次自检）
- `1`：有 WARN（版本漂移等）或 FAIL —— 禁止直接上生产

### 实测记录

| 场景 | 结果 |
|------|------|
| central-inbox（修复 definePlugin 后） | ✅ 1 OK / 0 WARN / 0 FAIL，exit=0 |
| agent-bus | ✅ 5 OK / 0 WARN / 0 FAIL，exit=0 |
| 坏插件（definePlugin + nonexistent-pkg） | ❌ 2 FAIL（API 漂移 + 解析链不可达），exit=1 |
| cordis 3.9.0 vs 期望 ^4.0.1 | ⚠️ 1 WARN（版本漂移），exit=1 |

### 设计要点

- 纯 std + serde_json，零外部 HTTP 依赖，不绑定端口（符合运维硬约束：不起服务测试）
- API 漂移检测**精确解析 export { A, B } 块**，不做宽松 `export {` 匹配（打包产物必然命中会误报）
- 自动探测宿主 CLD runtime（mac 常见路径）

## 附：central-inbox definePlugin 修复（同批）

- `~/dsh-plugin-central-inbox/lib/index.js` 删除 `import { definePlugin } from '@deepseek-ai/cordis'`（cordis 4.x 已移除，此前 import 但未使用靠 tree-shaking 摇掉才没崩——悬空 import 隐患）
- 已同步进完整底座交付包 v1.0.1

## 构建

```
cargo build --release                                    # macos-arm64
cargo zigbuild --release --target x86_64-pc-windows-gnu  # win-x64（需 zig）
cargo zigbuild --release --target x86_64-unknown-linux-gnu  # linux-x64
```

---

## v1.4.1 追加（2026-08-27 03:22）

按 MBP 部署安全方案（data/mac-mini/deploy-safety-scheme-v1）第二节四段静态审查补齐：

| 段 | 检查项 | 实现 | 级别 |
|----|--------|------|------|
| 1 | deps 用 link:/file: 语法 | 值前缀识别 | OK/WARN（registry 版本跨设备需确认）|
| 2 | bundles 顺序（依赖插件先于依赖它的插件）| `--profile <package.json>` 参数 | FAIL（顺序错）|
| 3 | dsh.bundle.patch 引用的 cordis.patch.yml 存在 | 文件存在性 | FAIL |
| 4 | lib/*.js 语法 | node --check（node 不可用则静默跳过）| FAIL |

**实测**：
- agent-bus 7 OK / 0 WARN / 0 FAIL（含 bundle.patch 存在 + bundles 顺序 agent-bus idx28 < central-inbox idx29）exit=0
- central-inbox 3 OK / 0 WARN / 0 FAIL exit=0
- 负向（交换 bundles 顺序）→ FAIL「bundles 顺序错误」exit=1

---

## v1.4.2（2026-08-27 03:25）版本比较修复

MBP 部署实战反馈：宿主依赖是 rc.8（比 mac 快照 rc.6 新），check-deps.sh 精确比较误报「版本不一致」。

**修复**：版本比较改为「宿主 ≥ 期望」（语义化，rc 感知）：
- 宿主更新（rc.8 >= rc.6）→ ✅ OK（宿主更新兼容，不降级）
- 宿主过旧（rc.5 < rc.6）→ ⚠️ WARN/FAIL（版本不满足）

同步修复 deploy-check 的 version_ok（原 major.minor 比较会把 rc.5 误判为满足 rc.6）。

**实测**：rc.8 vs rc.6 → OK exit=0；rc.5 vs rc.6 → WARN exit=1。
