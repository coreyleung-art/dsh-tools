// dsh-tools health-check — 插件健康检查 + 踩坑档案 + 升级工作流状态
// 吸收自：i9 dsh-plugin-health-check v0.1.0（genebank sha256:582a0df9...），Rust 化 + mac 路径适配
// 工具：health-check（plugin_health）/ upgrade-status（ops_upgrade_status）/ pitfall query|add
// 设计：纯 std + serde_json，与本工具集同风格；数据源对齐本机单一事实源
//   pitfalls.json: ~/dsh-collab/docs/pitfalls/pitfalls.json（items[] 格式）
//   CLD runtime:   /Applications/CLD.app/Contents/Resources/dsh-runtime/runtime（自动探测）
//   profile:       ~/.dsh/profiles/web/node_modules
//   central-inbox: ~/.dsh/central-inbox.log

use std::path::{Path, PathBuf};
use serde_json::{json, Value};

// ── 路径探测 ──

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/Users/unknown"))
}

fn find_runtime() -> PathBuf {
    // 自动探测 CLD runtime：常见安装位置
    let candidates = [
        "/Applications/CLD.app/Contents/Resources/dsh-runtime/runtime",
        "/Applications/CLD.app/Contents/Resources/dsh-runtime",
        "/Users/coreyleung/CLD/dsh-runtime/runtime",
    ];
    for c in candidates {
        if Path::new(c).join("node_modules/@deepseek-ai/dsh/package.json").exists() {
            return PathBuf::from(c);
        }
    }
    // 兜底：从 PATH 找 dsh bin 所在目录向上探测
    PathBuf::from("/Applications/CLD.app/Contents/Resources/dsh-runtime/runtime")
}

fn profile_nm() -> PathBuf {
    home().join(".dsh/profiles/web/node_modules")
}

fn read_json(p: &Path) -> Option<Value> {
    std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok())
}

fn tail_file(p: &Path, n: usize) -> Vec<String> {
    match std::fs::read_to_string(p) {
        Ok(t) => {
            let ls: Vec<&str> = t.lines().filter(|l| !l.trim().is_empty()).collect();
            let start = ls.len().saturating_sub(n);
            ls[start..].iter().map(|s| s.to_string()).collect()
        }
        Err(_) => vec!["(无日志)".to_string()],
    }
}

fn tcp_ok(host: &str, port: u16) -> bool {
    use std::net::TcpStream;
    use std::time::Duration;
    TcpStream::connect_timeout(&format!("{}:{}", host, port).parse().unwrap_or_else(|_| {
        std::net::SocketAddr::from(([127, 0, 0, 1], port))
    }), Duration::from_secs(3)).is_ok()
}

fn dsh_version(runtime: &Path) -> String {
    let p = runtime.join("node_modules/@deepseek-ai/dsh/package.json");
    match read_json(&p) {
        Some(v) => v.get("version").and_then(|x| x.as_str()).unwrap_or("?").to_string(),
        None => "MISSING".to_string(),
    }
}

// ── ① health-check（i9 plugin_health）──

pub fn run_health_check() -> Value {
    let runtime = find_runtime();
    let profile = profile_nm();
    let mut checks: Vec<Value> = Vec::new();

    // 1) dsh 版本
    let dsv = dsh_version(&runtime);
    checks.push(json!({"name": "dsh 版本", "ok": dsv != "MISSING", "detail": dsv}));

    // 2) 关键插件存在 + 模块可加载（动态扫描 profile node_modules 下的 dsh-plugin-*）
    let core_plugins = ["dsh-plugin-agent-way", "dsh-plugin-central-inbox", "dsh-plugin-bus-bridge"];
    let mut plugin_count = 0usize;
    let mut missing_core: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&profile) {
        for e in entries.flatten() {
            let nm = e.file_name().to_string_lossy().to_string();
            if !nm.starts_with("dsh-plugin-") { continue; }
            let pkg_path = profile.join(&nm).join("package.json");
            let pkg = read_json(&pkg_path);
            let main_ok = pkg.as_ref().and_then(|p| {
                let main = p.get("main").and_then(|m| m.as_str()).unwrap_or("lib/index.js");
                Some(profile.join(&nm).join(main).exists())
            }).unwrap_or(false);
            if pkg.is_some() { plugin_count += 1; }
            let is_core = core_plugins.contains(&nm.as_str());
            if is_core && pkg.is_none() { missing_core.push(nm.clone()); }
            checks.push(json!({
                "name": nm, "ok": pkg.is_some() && main_ok,
                "detail": format!("v{} main={}", pkg.as_ref().and_then(|p| p.get("version").and_then(|v| v.as_str())).unwrap_or("?"), if main_ok {"ok"} else {"MISSING"})
            }));
        }
    }
    for c in core_plugins.iter() {
        if !missing_core.iter().any(|m| m == c) { continue; }
        checks.push(json!({"name": *c, "ok": false, "detail": "核心插件缺失"}));
    }

    // 3) SSE 8803 连接（事件桥存活）
    let sse_ok = tcp_ok("127.0.0.1", 8803);
    checks.push(json!({"name": "SSE 8803 事件桥", "ok": sse_ok, "detail": if sse_ok {"连接正常"} else {"无监听（黑板未运行或 SSE 桥未启）"}}));

    // 4) central-inbox 最近活动
    let ci_log = home().join(".dsh/central-inbox.log");
    if ci_log.exists() {
        let last = tail_file(&ci_log, 5);
        let active = last.iter().any(|l| {
            l.contains("inject OK") || l.contains("SSE connected") || l.contains("apply start")
                || l.contains("inject") || l.contains("注入") || l.contains("delivered") || l.contains("deliv")
        });
        checks.push(json!({"name": "central-inbox 活动", "ok": active, "detail": last.last().cloned().unwrap_or_default().chars().take(100).collect::<String>()}));
    } else {
        checks.push(json!({"name": "central-inbox 活动", "ok": false, "detail": "日志不存在（central-inbox 未运行）"}));
    }

    // 5) agentBus 服务
    let bus = home().join(".dsh/agent-bus.json");
    let bus_ok = bus.exists();
    checks.push(json!({"name": "agentBus 总线", "ok": bus_ok, "detail": if bus_ok {"agent-bus.json 存在"} else {"agent-bus.json 缺失"}}));

    let all_ok = checks.iter().all(|c| c.get("ok").and_then(|o| o.as_bool()).unwrap_or(false));
    json!({
        "ts": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "dsh_version": dsv,
        "plugins_found": plugin_count,
        "checks": checks,
        "all_ok": all_ok,
        "runtime": runtime.display().to_string(),
    })
}

// ── ② upgrade-status（i9 ops_upgrade_status）──

fn run_upgrade_status() -> Value {
    let runtime = find_runtime();
    let dsv = dsh_version(&runtime);
    let mut status = json!({
        "ts": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "dsh_version": dsv,
    });

    // 沙箱/压测日志（本机 scripts 目录 + ~/.cld/logs）
    let log_sources: Vec<(&str, PathBuf)> = vec![
        ("sandbox", home().join("dsh-collab/scripts/cld-shell-sandbox-test.log")),
        ("stress", home().join("dsh-collab/scripts/restart-stress-test.log")),
        ("cld_log", home().join(".cld/logs/dsh-web.log")),
        ("crash_reason", home().join(".cld/logs/crash-reason.log")),
    ];
    let mut logs = json!({});
    for (name, p) in log_sources {
        logs[name] = json!(tail_file(&p, 3));
    }
    status["logs"] = logs;

    // restart-gate 存在性
    status["restart_gate_available"] = json!(home().join("dsh-collab/rust-tools/target/release/dsh-tools").exists());
    status["pitfalls_count"] = json!(match read_json(&pitfalls_path()) {
        Some(v) => v.get("items").and_then(|i| i.as_array()).map(|a| a.len()).unwrap_or(0),
        None => 0,
    });
    status
}

// ── ③ pitfall query（i9 pitfall_query）──

fn pitfalls_path() -> PathBuf {
    home().join("dsh-collab/docs/pitfalls/pitfalls.json")
}

fn load_pitfalls() -> Value {
    read_json(&pitfalls_path()).unwrap_or_else(|| json!({"version": 1, "items": []}))
}

fn cmd_pitfall_query(args: &[String]) {
    let mut tag: Option<String> = None;
    let mut level: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--tag" => { i += 1; if i < args.len() { tag = Some(args[i].clone()); } }
            "--level" => { i += 1; if i < args.len() { level = Some(args[i].clone()); } }
            s if !s.starts_with('-') && tag.is_none() => tag = Some(s.to_string()),
            _ => {}
        }
        i += 1;
    }
    let data = load_pitfalls();
    let items = data.get("items").and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let mut hits: Vec<Value> = items;
    if let Some(t) = &tag {
        let tl = t.to_lowercase();
        hits.retain(|e| {
            let hay = vec![
                e.get("title").and_then(|x| x.as_str()).unwrap_or(""),
                e.get("root_cause").and_then(|x| x.as_str()).unwrap_or(""),
                e.get("fix").and_then(|x| x.as_str()).unwrap_or(""),
                e.get("id").and_then(|x| x.as_str()).unwrap_or(""),
            ].join(" ").to_lowercase();
            hay.contains(&tl)
        });
    }
    if let Some(lv) = &level {
        hits.retain(|e| e.get("severity").and_then(|x| x.as_str()).unwrap_or("").eq_ignore_ascii_case(lv));
    }
    let out = json!({"count": hits.len(), "hits": hits.iter().take(20).collect::<Vec<_>>()});
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

// ── ④ pitfall add（i9 pitfall_add）──

fn cmd_pitfall_add(args: &[String]) {
    let mut title: Option<String> = None;
    let mut severity = "P2".to_string();
    let mut date: Option<String> = None;
    let mut detail: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--level" | "--severity" => { i += 1; if i < args.len() { severity = args[i].clone(); } }
            "--date" => { i += 1; if i < args.len() { date = Some(args[i].clone()); } }
            "--detail" => { i += 1; if i < args.len() { detail = Some(args[i].clone()); } }
            s if !s.starts_with('-') && title.is_none() => title = Some(s.to_string()),
            _ => {}
        }
        i += 1;
    }
    let title = match title {
        Some(t) => t,
        None => { println!("用法: dsh-tools pitfall add <title> [--level P1|P2|P3] [--date YYYY-MM-DD] [--detail 坑/根因/修复]"); return; }
    };
    let date = date.unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());
    // ⚠️ 写入需遵守红绿灯协议（R001）：调用方应先 agent_lock file:pitfalls.json
    let mut data = load_pitfalls();
    let items = data.get_mut("items").and_then(|x| x.as_array_mut()).unwrap();
    // id：英文标题 → date-slug；中文/空 slug → date-P<序号>（兜底，避免空 id）
    let slugged = slug(&title);
    let id = if slugged.is_empty() {
        format!("{}-P{}", date, items.len() + 1)
    } else {
        format!("{}-{}", date, slugged)
    };
    let entry = json!({
        "id": id,
        "severity": severity,
        "date": date,
        "title": title,
        "phenomenon": detail.clone().unwrap_or_default(),
        "root_cause": "",
        "fix": "",
        "lesson": "",
        "sop_update": ""
    });
    items.push(entry.clone());
    let ok = std::fs::write(pitfalls_path(), serde_json::to_string_pretty(&data).unwrap_or_default()).is_ok();
    println!("{}", serde_json::to_string_pretty(&json!({
        "ok": ok, "id": id, "entry": entry,
        "note": "已写入 ~/dsh-collab/docs/pitfalls/pitfalls.json（单一事实源）。写前应 agent_lock file:pitfalls.json（R001 红绿灯）"
    })).unwrap_or_default());
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            out.push(ch.to_ascii_lowercase());
        } else if ch == ' ' || ch == '/' || ch == ':' {
            out.push('-');
        }
    }
    while out.contains("--") { out = out.replace("--", "-"); }
    out.trim_matches('-').to_string()
}

// ── 入口 ──

pub fn run(args: &[String]) -> i32 {
    if args.is_empty() {
        println!("dsh-tools health 子命令:");
        println!("  health-check                     — 插件健康检查（dsh 版本/插件/SSE/注入/总线）");
        println!("  upgrade-status                   — 升级链路状态（沙箱/压测/崩溃日志 + 版本）");
        println!("  pitfall query <关键词> [--level P1] — 查询踩坑档案");
        println!("  pitfall add <标题> [--level P1] [--date YYYY-MM-DD] [--detail 内容] — 登记踩坑（写前红绿灯）");
        return 0;
    }
    match args[0].as_str() {
        "health-check" => {
            println!("{}", serde_json::to_string_pretty(&run_health_check()).unwrap_or_default());
        }
        "upgrade-status" => {
            println!("{}", serde_json::to_string_pretty(&run_upgrade_status()).unwrap_or_default());
        }
        "pitfall" if args.len() >= 2 && args[1] == "query" => cmd_pitfall_query(&args[2..]),
        "pitfall" if args.len() >= 2 && args[1] == "add" => cmd_pitfall_add(&args[2..]),
        "pitfall" => { println!("用法: dsh-tools health pitfall query|add ..."); }
        other => println!("未知 health 子命令: {}", other),
    }
    0
}
