// dsh-tools deploy-check — 跨设备插件部署安全审查（沙箱隔离模拟机制 v1.0）
// 依据：MBP 部署 agent-bus/central-inbox 实战（缺传递依赖/definePlugin API 漂移/解析链不可达会崩）
// 功能：一键静态审查 + 依赖解析循环补包 + 自检报告
// 用法：dsh-tools deploy-check <插件目录> [--runtime <CLD runtime 路径>] [--json]
// 设计：纯 std + serde_json，不绑定端口（硬约束），只做静态分析与版本核对

use std::collections::BTreeMap;
use std::path::Path;

/// 一个待检查项的结果
struct CheckItem {
    level: &'static str, // "OK" / "WARN" / "FAIL"
    msg: String,
}

/// deploy-check 主入口
pub fn run(args: &[String]) -> i32 {
    let mut plugin_dir = String::new();
    let mut runtime = String::new();
    let mut profile_arg: Option<String> = None;
    let mut json_out = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--runtime" => { i += 1; if i < args.len() { runtime = args[i].clone(); } }
            "--profile" => { i += 1; if i < args.len() { profile_arg = Some(args[i].clone()); } }
            "--json" => { json_out = true; }
            s if !s.starts_with('-') && plugin_dir.is_empty() => { plugin_dir = s.to_string(); }
            _ => {}
        }
        i += 1;
    }

    if plugin_dir.is_empty() {
        println!("用法: dsh-tools deploy-check <插件目录> [--runtime <CLD runtime>] [--profile <profile/package.json>] [--json]");
        println!("  <插件目录>: 要审查的插件源码目录（含 lib/ 与 package.json）");
        println!("  --runtime: 宿主 CLD runtime 的 node_modules 父目录（默认自动探测）");
        println!("  --profile: 宿主 profile 的 package.json 路径（检查 bundles 顺序，可选）");
        return 1;
    }

    // ── 探测宿主 runtime ──
    let runtime = if runtime.is_empty() { detect_runtime() } else { runtime };
    let nm = Path::new(&runtime).join("node_modules");

    let mut checks: Vec<CheckItem> = Vec::new();
    let dir = Path::new(&plugin_dir);

    // ① 基础结构检查
    let lib_dir = dir.join("lib");
    let pkg_path = dir.join("package.json");
    if !lib_dir.is_dir() {
        checks.push(CheckItem { level: "FAIL", msg: format!("缺 lib/ 目录: {}", dir.display()) });
    }
    if !pkg_path.is_file() {
        checks.push(CheckItem { level: "FAIL", msg: format!("缺 package.json: {}", pkg_path.display()) });
    } else {
        // ② package.json 解析：name / peerDependencies / dependencies
        match std::fs::read_to_string(&pkg_path) {
            Ok(txt) => match serde_json::from_str::<serde_json::Value>(&txt) {
                Ok(pkg) => {
                    let name = pkg.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    checks.push(CheckItem { level: "OK", msg: format!("插件名: {}", name) });
                    // peerDependencies = 宿主必须提供的包
                    if let Some(peers) = pkg.get("peerDependencies").and_then(|v| v.as_object()) {
                        for (p, ver) in peers {
                            check_host_pkg(&nm, p, ver.as_str().unwrap_or(""), &mut checks);
                        }
                    }
                    // dependencies = 插件自带/需补的包
                    if let Some(deps) = pkg.get("dependencies").and_then(|v| v.as_object()) {
                        if deps.is_empty() {
                            checks.push(CheckItem { level: "OK", msg: "dependencies 为空（全部走宿主 peer）".to_string() });
                        }
                        for (p, ver) in deps {
                            check_host_pkg(&nm, p, ver.as_str().unwrap_or(""), &mut checks);
                        }
                    }
                }
                Err(e) => checks.push(CheckItem { level: "FAIL", msg: format!("package.json 解析失败: {}", e) }),
            },
            Err(e) => checks.push(CheckItem { level: "FAIL", msg: format!("读 package.json 失败: {}", e) }),
        }
    }

    // ③ 扫描 lib/*.js 的 import/require（外部依赖 + 常见 API 漂移）
    if lib_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&lib_dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "js").unwrap_or(false) {
                    scan_js_file(&p, &nm, &mut checks);
                }
            }
        }
    }

    // ④ MBP 方案第二节四段静态审查补充
    //    段1: package.json deps 用 link:/file: 语法（打包/安装约定）
    if let Ok(txt) = std::fs::read_to_string(&pkg_path) {
        if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&txt) {
            if let Some(deps) = pkg.get("dependencies").and_then(|v| v.as_object()) {
                for (p, ver) in deps {
                    let v = ver.as_str().unwrap_or("");
                    if v.starts_with("link:") || v.starts_with("file:") || v.starts_with("github:") {
                        checks.push(CheckItem { level: "OK", msg: format!("deps[{}] 用 {} 语法（本地/远程引用）", p, v) });
                    } else if !v.is_empty() && v != "*" {
                        checks.push(CheckItem { level: "WARN", msg: format!("deps[{}] 用 registry 版本 {} —— 跨设备需确认宿主已装同版本", p, v) });
                    }
                }
            }
            //    段3: dsh.bundle.patch 引用的 cordis.patch.yml 存在
            if let Some(patch) = pkg.get("dsh").and_then(|d| d.get("bundle")).and_then(|b| b.get("patch")).and_then(|v| v.as_str()) {
                let patch_path = dir.join(patch);
                if patch_path.is_file() {
                    checks.push(CheckItem { level: "OK", msg: format!("bundle.patch → {} 存在", patch) });
                } else {
                    checks.push(CheckItem { level: "FAIL", msg: format!("bundle.patch 引用的 {} 不存在（配置不生效）", patch) });
                }
            }
        }
    }

    //    段2: bundles 顺序（依赖插件先于依赖它的插件）——需 --profile 提供宿主 profile package.json
    if let Some(profile) = &profile_arg {
        check_bundle_order(profile, &mut checks);
    }

    //    段4: lib/*.js 语法检查（node --check）
    if lib_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&lib_dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "js").unwrap_or(false) {
                    let fname = p.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                    let node_bin = ["node", "/opt/homebrew/bin/node", "/usr/local/bin/node"]
                        .iter().find(|c| std::process::Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))
                        .map(|s| s.to_string()).unwrap_or_else(|| "node".to_string());
                    match std::process::Command::new(&node_bin).args(["--check"]).arg(&p).output() {
                        Ok(out) if out.status.success() => {
                            checks.push(CheckItem { level: "OK", msg: format!("[{}] 语法 OK", fname) });
                        }
                        Ok(out) => {
                            let err = String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("?").to_string();
                            checks.push(CheckItem { level: "FAIL", msg: format!("[{}] 语法错误: {}", fname, err) });
                        }
                        Err(_) => { /* node 不可用则跳过（不误报） */ }
                    }
                }
            }
        }
    }

    // ⑤ 汇总报告
    let ok = checks.iter().filter(|c| c.level == "OK").count();
    let warn = checks.iter().filter(|c| c.level == "WARN").count();
    let fail = checks.iter().filter(|c| c.level == "FAIL").count();

    if json_out {
        let items: Vec<serde_json::Value> = checks.iter()
            .map(|c| serde_json::json!({"level": c.level, "msg": c.msg}))
            .collect();
        let out = serde_json::json!({
            "plugin_dir": plugin_dir,
            "runtime": runtime,
            "summary": {"ok": ok, "warn": warn, "fail": fail},
            "checks": items
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        println!("══ deploy-check · 部署安全审查 ══");
        println!("插件: {}  宿主 runtime: {}", plugin_dir, runtime);
        println!("{}", "─".repeat(52));
        for c in &checks {
            let mark = match c.level { "OK" => "✅", "WARN" => "⚠️ ", _ => "❌" };
            println!(" {} {} {}", mark, c.level, c.msg);
        }
        println!("{}", "─".repeat(52));
        println!("结果: {} OK / {} WARN / {} FAIL", ok, warn, fail);
        if fail == 0 && warn == 0 {
            println!("✅ 通过 —— 可安全部署（仍需收包端二次自检）");
        } else if fail == 0 {
            println!("⚠️  WARN 存在（版本漂移等）—— 建议修复后再上生产；紧急可用 --force 绕过");
        } else {
            println!("❌ 未通过 —— 修复后再发包（禁止直接上生产）");
        }
    }
    if fail == 0 && warn == 0 { 0 } else { 1 }
}

/// 自动探测宿主 CLD runtime（mac 常见路径）
fn detect_runtime() -> String {
    let candidates = [
        "/Applications/CLD.app/Contents/Resources/dsh-runtime/runtime",
        "/Applications/CLD.app/Contents/Resources/dsh-runtime/runtime",
    ];
    for c in candidates {
        if Path::new(c).join("node_modules").is_dir() {
            return c.to_string();
        }
    }
    ".".to_string()
}

/// 核对一个宿主包是否存在且版本符合
fn check_host_pkg(nm: &Path, pkg_name: &str, want: &str, checks: &mut Vec<CheckItem>) {
    let pj = nm.join(pkg_name).join("package.json");
    if !pj.is_file() {
        checks.push(CheckItem {
            level: "FAIL",
            msg: format!("宿主缺依赖包: {} (期望 {})——解析链不可达", pkg_name, if want.is_empty() { "*" } else { want }),
        });
        return;
    }
    match std::fs::read_to_string(&pj) {
        Ok(txt) => match serde_json::from_str::<serde_json::Value>(&txt) {
            Ok(v) => {
                let got = v.get("version").and_then(|x| x.as_str()).unwrap_or("?");
                let note = if want.is_empty() || version_ok(&got, want) {
                    format!("宿主 {} @ {} （期望 {}）", pkg_name, got, if want.is_empty() { "任意" } else { want })
                } else {
                    format!("宿主 {} @ {} ≠ 期望 {} —— 版本漂移", pkg_name, got, want)
                };
                let bad = !want.is_empty() && !version_ok(&got, want);
                checks.push(CheckItem { level: if bad { "WARN" } else { "OK" }, msg: note });
            }
            Err(e) => checks.push(CheckItem { level: "WARN", msg: format!("{} package.json 解析失败: {}", pkg_name, e) }),
        },
        Err(_) => checks.push(CheckItem { level: "WARN", msg: format!("读 {} package.json 失败", pkg_name) }),
    }
}

/// 语义版本比较：got >= want（支持 x.y.z / x.y.z-rc.N；rc.N 越大越新）
/// 宿主更新（rc.8 >= rc.6）→ OK；宿主过旧（rc.5 < rc.6）→ 不满足
fn version_ok(got: &str, want: &str) -> bool {
    let w = want.trim();
    if w.is_empty() || w == "*" { return true; }
    let w = w.trim_start_matches(['^', '~', '>', '=', ' ']);
    let g = got.trim_start_matches(['^', '~', '>', '=', ' ', 'v']);
    let w = w.trim_start_matches('v');

    // 提取主版本（点分数字）+ rc 号
    fn parse(v: &str) -> (Vec<u64>, u64) {
        let mut rc = 999;
        let mut core = v;
        if let Some(idx) = v.find("-rc.") {
            if let Some(r) = v[idx + 4..].split('.').next().and_then(|s| s.parse().ok()) {
                rc = r;
            }
            core = &v[..idx];
        }
        let nums: Vec<u64> = core.split('.').filter_map(|s| s.parse().ok()).collect();
        (nums, rc)
    }
    let (wv, wrc) = parse(w);
    let (gv, grc) = parse(g);
    if wv.is_empty() || gv.is_empty() { return true; }
    // 逐位比较主版本
    let len = wv.len().max(gv.len());
    for i in 0..len {
        let a = gv.get(i).copied().unwrap_or(0);
        let b = wv.get(i).copied().unwrap_or(0);
        if a > b { return true; }
        if a < b { return false; }
    }
    // 主版本相等 → 比 rc（宿主 >= 期望）
    grc >= wrc
}

/// 扫描单个 JS 文件的 import/require 外部模块 + API 漂移
fn scan_js_file(path: &Path, nm: &Path, checks: &mut Vec<CheckItem>) {
    let txt = match std::fs::read_to_string(path) { Ok(t) => t, Err(_) => return };
    let fname = path.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
    let mut mods: BTreeMap<String, Vec<String>> = BTreeMap::new(); // 模块 -> 具名导入

    for line in txt.lines() {
        let l = line.trim();
        // import { a, b } from 'pkg';  /  import pkg from 'pkg';
        if l.starts_with("import ") && l.contains("from '") {
            let (_, rest) = l.split_once("from '").unwrap_or(("", ""));
            let m = rest.split('\'').next().unwrap_or("").to_string();
            if !m.is_empty() && !m.starts_with('.') && !m.starts_with("node:") {
                let named = extract_named_imports(l);
                mods.entry(m).or_default().extend(named);
            }
        }
        // require('pkg')
        if l.contains("require('") {
            let m = l.split("require('").nth(1).and_then(|s| s.split('\'').next()).unwrap_or("").to_string();
            if !m.is_empty() && !m.starts_with('.') && !m.starts_with("node:") {
                mods.entry(m).or_default();
            }
        }
    }

    for (m, named) in &mods {
        // node: 内置模块跳过
        if m.starts_with("node:") { continue; }
        let pj = nm.join(m).join("package.json");
        if !pj.is_file() {
            checks.push(CheckItem {
                level: "FAIL",
                msg: format!("[{}] import '{}' → 宿主无此包（解析链不可达）", fname, m),
            });
            continue;
        }
        // API 漂移检测：具名导入是否真的存在（如 definePlugin 在 cordis 4.x 已移除）
        for n in named {
            if n == "default" { continue; }
            if !export_exists(&pj, n) {
                checks.push(CheckItem {
                    level: "FAIL",
                    msg: format!("[{}] import {{ {} }} from '{}' → API 漂移：宿主包未导出 {}（cordis 4.x 已移除 definePlugin 等）", fname, n, m, n),
                });
            }
        }
    }
}

/// 提取 import { a, b, c } 的具名列表
fn extract_named_imports(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(start) = line.find("import {") {
        let rest = &line[start + "import {".len()..];
        if let Some(end) = rest.find('}') {
            let body = &rest[..end];
            for part in body.split(',') {
                let name = part.trim().split(" as ").next().unwrap_or("").trim().to_string();
                if !name.is_empty() { out.push(name); }
            }
        }
    }
    out
}

/// 检查宿主包的导出中是否含某具名（宽松匹配：lib/index.js 里出现 `export` 或 `var name`）
fn export_exists(pkg_json: &Path, export_name: &str) -> bool {
    // 尝试解析 main 字段定位入口
    let entry = match std::fs::read_to_string(pkg_json) {
        Ok(t) => serde_json::from_str::<serde_json::Value>(&t)
            .ok()
            .and_then(|v| v.get("main").and_then(|x| x.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| "lib/index.js".to_string()),
        Err(_) => return true, // 读不到 package.json 不误报
    };
    let base = pkg_json.parent().unwrap_or(Path::new("."));
    let entry_path = base.join(&entry);
    let txt = match std::fs::read_to_string(&entry_path) {
        Ok(t) => t,
        Err(_) => {
            // 尝试 lib/index.js / index.js
            let alt = base.join("lib/index.js");
            if let Ok(t) = std::fs::read_to_string(&alt) { t } else { return true; }
        }
    };
    // 精确匹配导出。关键：不能宽松匹配 "export {"（打包产物必然命中），必须解析具名列表
    // 匹配形态：export { A, B, C }（单个块，可跨行） / export const X / export function X / exports.X =
    let block_hit = txt.lines().any(|l| {
        let l = l.trim();
        if !(l.starts_with("export {") && l.ends_with('}')) { return false; }
        let body = &l["export {".len()..l.len() - 1];
        body.split(',').any(|part| {
            let p = part.trim();
            if p.is_empty() { return false; }
            // 处理 "原名 as 别名" 与 ascii 转义（\uXXXX）
            let orig = p.split(" as ").next().unwrap_or("").trim().trim_start_matches('\\');
            orig == export_name || p == export_name
        })
    });
    block_hit
        || txt.contains(&format!("export const {}", export_name))
        || txt.contains(&format!("export function {}", export_name))
        || txt.contains(&format!("export class {}", export_name))
        || txt.contains(&format!("export {} =", export_name))
        || txt.contains(&format!("exports.{} =", export_name))
        || txt.contains(&format!("var {} =", export_name))
        || txt.contains(&format!("function {}(", export_name))
        || txt.contains(&format!("const {} =", export_name))
}

/// 打包前自检（本机运行）
pub fn self_check() {
    println!("deploy-check 自检: 编译通过，模块可加载");
}

/// MBP 方案段2：bundles 顺序检查（依赖插件必须先于依赖它的插件）
/// 已知依赖序：agent-bus 在 central-inbox 前（central-inbox inject ['agentBus']）
fn check_bundle_order(profile_pkg: &str, checks: &mut Vec<CheckItem>) {
    let txt = match std::fs::read_to_string(profile_pkg) {
        Ok(t) => t,
        Err(_) => {
            checks.push(CheckItem { level: "WARN", msg: format!("--profile 指定文件不可读: {}（跳过顺序检查）", profile_pkg) });
            return;
        }
    };
    let pkg: serde_json::Value = match serde_json::from_str(&txt) {
        Ok(v) => v,
        Err(_) => {
            checks.push(CheckItem { level: "WARN", msg: "profile package.json 解析失败（跳过顺序检查）".to_string() });
            return;
        }
    };
    let bundles: Vec<String> = pkg.get("dsh").and_then(|d| d.get("profile"))
        .and_then(|p| p.get("bundles")).and_then(|b| b.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    if bundles.is_empty() {
        checks.push(CheckItem { level: "WARN", msg: "profile 无 dsh.profile.bundles（跳过顺序检查）".to_string() });
        return;
    }
    // 检查已登记的依赖序对
    let deps_order: [(&str, &str); 1] = [("dsh-plugin-agent-way", "dsh-plugin-central-inbox")];
    for (dep, uses) in &deps_order {
        let di = bundles.iter().position(|b| b == dep);
        let ui = bundles.iter().position(|b| b == uses);
        match (di, ui) {
            (Some(d), Some(u)) if d < u => {
                checks.push(CheckItem { level: "OK", msg: format!("bundles 顺序 OK: {} (idx {}) 先于 {} (idx {})", dep, d, uses, u) });
            }
            (Some(d), Some(u)) => {
                checks.push(CheckItem { level: "FAIL", msg: format!("bundles 顺序错误: {} (idx {}) 在 {} (idx {}) 之后——依赖插件必须先于依赖它的插件", dep, d, uses, u) });
            }
            (None, Some(_)) => {
                checks.push(CheckItem { level: "WARN", msg: format!("{} 未在 bundles 中（{} 依赖它，可能加载失败）", dep, uses) });
            }
            _ => {}
        }
    }
}

/// restart-guard — CLD/DSH 重启前沙箱模拟强制门（R011）
/// 用法: dsh-tools restart-guard <插件目录>... [--runtime R] [--profile P] [--json]
/// 在 deploy-check 全部检测之上，追加「重启专属崩溃类」检测（2026-08-29 实战教训）：
///   ⑧ type:module 检查（ESM export 但无 type:module → CJS 解析 SyntaxError → 插件加载即崩）
///   ⑨ ESM 导入完整性（join/homedir/fs 等依赖 CJS 隐式全局 → ESM 下 ReferenceError）
///   ⑩ node_modules 符号链接（agentBus 等 peer 依赖解析）
///   ⑪ 模块加载实测（node import() 模拟 cordis 加载）
/// 任一项 FAIL → 返回非 0（强制门：禁止重启）
pub fn restart_guard(args: &[String]) -> i32 {
    let mut dirs: Vec<String> = Vec::new();
    let mut runtime = String::new();
    let mut profile_arg: Option<String> = None;
    let mut json_out = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--runtime" => { i += 1; if i < args.len() { runtime = args[i].clone(); } }
            "--profile" => { i += 1; if i < args.len() { profile_arg = Some(args[i].clone()); } }
            "--json" => { json_out = true; }
            s if !s.starts_with('-') => { dirs.push(s.to_string()); }
            _ => {}
        }
        i += 1;
    }
    if dirs.is_empty() {
        println!("用法: dsh-tools restart-guard <插件目录>... [--runtime R] [--profile P] [--json]");
        println!("  CLD/DSH 重启前强制门：任一插件 FAIL → 禁止重启（沙箱先行，R011）");
        return 1;
    }
    let runtime = if runtime.is_empty() { detect_runtime() } else { runtime };
    let nm = Path::new(&runtime).join("node_modules");

    let mut checks: Vec<CheckItem> = Vec::new();
    for d in &dirs {
        let dir = Path::new(d);
        let pkg_path = dir.join("package.json");
        checks.push(CheckItem { level: "INFO", msg: format!("══ restart-guard 审查: {}", dir.display()) });
        if !dir.is_dir() {
            checks.push(CheckItem { level: "FAIL", msg: format!("目录不存在: {}", dir.display()) });
            continue;
        }
        // ⑧ type:module 检查（ESM export 必须配 type:module）
        match std::fs::read_to_string(&pkg_path) {
            Ok(txt) => {
                if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&txt) {
                    let t = pkg.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    // 检测 lib/index.js 是否用 ESM 语法（export/import），而非 package.json 文本
                    let main_js = dir.join("lib").join("index.js");
                    let mut has_esm = false;
                    if let Ok(main_txt) = std::fs::read_to_string(&main_js) {
                        has_esm = main_txt.contains("export ") || main_txt.contains("import ");
                    }
                    if has_esm && t != "module" {
                        checks.push(CheckItem { level: "FAIL", msg: format!("[{}] lib/index.js 用 ESM 语法但缺 type:module（CJS 解析 → SyntaxError → 加载即崩）", d) });
                    } else if has_esm && t == "module" {
                        checks.push(CheckItem { level: "OK", msg: format!("[{}] type:module + ESM 语法匹配", d) });
                    } else if !has_esm && t == "module" {
                        checks.push(CheckItem { level: "WARN", msg: format!("[{}] type:module 但 lib 无 ESM 语法（无害，仅提示）", d) });
                    } else {
                        checks.push(CheckItem { level: "OK", msg: format!("[{}] CJS 插件（无 ESM 语法）", d) });
                    }
                    // peerDependencies 非空（agentBus 等服务依赖声明）
                    let peers = pkg.get("peerDependencies").and_then(|v| v.as_object()).map(|o| o.len()).unwrap_or(0);
                    if peers == 0 && has_esm {
                        checks.push(CheckItem { level: "WARN", msg: format!("[{}] peerDependencies 为空（注入服务可能无法解析，建议声明）", d) });
                    } else if peers > 0 {
                        checks.push(CheckItem { level: "OK", msg: format!("[{}] peerDependencies {} 项", d, peers) });
                    }
                } else {
                    checks.push(CheckItem { level: "FAIL", msg: format!("[{}] package.json 解析失败", d) });
                }
            }
            Err(e) => checks.push(CheckItem { level: "FAIL", msg: format!("[{}] 读 package.json 失败: {}", d, e) }),
        }
        // ⑨⑩ lib/*.js ESM 导入完整性 + 符号链接
        let lib_dir = dir.join("lib");
        if lib_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&lib_dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.extension().map(|x| x == "js").unwrap_or(false) {
                        if let Ok(src) = std::fs::read_to_string(&p) {
                            let fname = p.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                            // 跳过客户端 web 文件（__ModuleLoader__/window. 格式：浏览器 require("react") 是客户端模块系统，非宿主 ESM）
                            if src.contains("__ModuleLoader__") || src.contains("window.") {
                                continue;
                            }
                            // 检查 CJS 隐式全局依赖（join/homedir/fs 未导入却在 ESM 下用）
                            // 只匹配「裸函数调用」（前面是空白/括号/等号/冒号/逗号），排除成员调用（x.join( / a.b.join(）
                            let joined_src = src.clone();
                            for sym in ["join(", "homedir(", "require('fs')", "require(\"fs\")"] {
                                let base = if sym == "join(" { "join" } else if sym == "homedir(" { "homedir" } else { "require" };
                                let mut found_bare = false;
                                let mut search_from = 0;
                                while let Some(pos) = joined_src[search_from..].find(sym) {
                                    let abs = search_from + pos;
                                    // 前一个非空白字符：若是 . 或字母数字_ 则为成员调用/标识符一部分，跳过
                                    let prev_non_space = joined_src[..abs].trim_end().chars().last().unwrap_or(' ');
                                    let is_member = matches!(prev_non_space, '.' | 'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '$');
                                    if !is_member { found_bare = true; break; }
                                    search_from = abs + sym.len();
                                }
                                // 宽松匹配：import { join, dirname } 等多名字导入也算已导入
                                let has_import = |sym: &str| -> bool {
                                    src.contains(&format!("import {{ {} }}", sym)) ||       // import { join }
                                    src.contains(&format!("import {{ {} ,", sym)) ||        // import { join, dirname }
                                    src.contains(&format!("import {{ {} ,", sym)) ||        // 覆盖 import { join, ...}
                                    src.lines().any(|l| l.contains("import") && l.contains(sym) && l.contains("node:path")) ||  // 从 node:path 导入含 sym
                                    src.lines().any(|l| l.contains("import") && l.contains(sym) && l.contains("node:os"))       // 从 node:os 导入含 sym
                                };
                                if found_bare && base == "join" && !has_import("join") {
                                    checks.push(CheckItem { level: "FAIL", msg: format!("[{}] 用了裸 join( 但缺 ESM 导入（type:module 下 ReferenceError）", fname) });
                                }
                                if found_bare && base == "homedir" && !has_import("homedir") {
                                    checks.push(CheckItem { level: "FAIL", msg: format!("[{}] 用了裸 homedir( 但缺 ESM 导入（type:module 下 ReferenceError）", fname) });
                                }
                                if found_bare && base == "require" {
                                    // require 可能是 createRequire 结果（module/node:module 显式导入），宽松判断
                                    let has_create_require = src.contains("createRequire")
                                        && (src.contains("node:module") || src.lines().any(|l| l.contains("import") && l.contains("createRequire")));
                                    if !has_create_require {
                                        checks.push(CheckItem { level: "FAIL", msg: format!("[{}] 用了裸 require( 但缺 ESM 导入（type:module 下 ReferenceError）", fname) });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // node_modules/@deepseek-ai 符号链接
            let nm_link = dir.join("node_modules").join("@deepseek-ai");
            if nm_link.is_dir() {
                let count = std::fs::read_dir(&nm_link).map(|r| r.flatten().count()).unwrap_or(0);
                checks.push(CheckItem { level: "OK", msg: format!("[{}] node_modules/@deepseek-ai 符号链接 {} 个", d, count) });
            } else {
                checks.push(CheckItem { level: "WARN", msg: format!("[{}] 无 node_modules/@deepseek-ai 符号链接（peer 依赖可能解析失败）", d) });
            }
        }
        // ⑪ 模块加载实测（node import() 模拟 cordis 加载）
        let main_js = dir.join("lib").join("index.js");
        if main_js.is_file() {
            // node 路径探测：PATH → /opt/homebrew/bin → /usr/local/bin → CLD runtime
            let node_bin = ["node", "/opt/homebrew/bin/node", "/usr/local/bin/node"]
                .iter().find(|c| std::process::Command::new(c).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))
                .map(|s| s.to_string()).unwrap_or_else(|| "node".to_string());
            let script = format!(
                "import('{}').then(m => console.log('LOAD_OK:' + Object.keys(m).join(','))).catch(e => {{ console.log('LOAD_FAIL:' + (e.message||'').slice(0,120)); process.exit(1); }})",
                main_js.display()
            );
            match std::process::Command::new(&node_bin).args(["-e", &script]).output() {
                Ok(out) => {
                    let out_txt = String::from_utf8_lossy(&out.stdout).to_string();
                    if out.status.success() && out_txt.contains("LOAD_OK") {
                        checks.push(CheckItem { level: "OK", msg: format!("[{}] 模块加载实测 OK: {}", d, out_txt.trim()) });
                    } else {
                        let err = String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("?").to_string();
                        checks.push(CheckItem { level: "FAIL", msg: format!("[{}] 模块加载实测失败: {}", d, err) });
                    }
                }
                Err(_) => checks.push(CheckItem { level: "WARN", msg: format!("[{}] node 不可用，跳过加载实测", d) }),
            }
        }
    }

    // 汇总
    let fail = checks.iter().filter(|c| c.level == "FAIL").count();
    let warn = checks.iter().filter(|c| c.level == "WARN").count();
    let ok = checks.iter().filter(|c| c.level == "OK").count();
    for c in &checks {
        if json_out {
            println!("{{\"level\":\"{}\",\"msg\":\"{}\"}}", c.level, c.msg.replace('"', "\\\""));
        } else {
            println!("[{}] {}", c.level, c.msg);
        }
    }
    println!("══ restart-guard 汇总: {} FAIL / {} WARN / {} OK ══", fail, warn, ok);
    if fail > 0 {
        println!("❌ 强制门: 存在 FAIL，禁止重启 CLD/DSH（R011 沙箱先行）");
        1
    } else {
        println!("✅ 强制门: 通过，可重启（R011）");
        0
    }
}
