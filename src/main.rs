// dsh-tools — 黑板固化工具集（Rust 版）
// 子命令:
//   bb-read <key> [--full] [--prefix P --last N] [--scan FILE]  — 看黑板摘要（本地模型）
//   bb-sub  --agent NAME --prefixes "a,b,c"                      — 黑板事件桥常驻订阅器
//   handoff --name X --files "a b" --to i9 --type ocr [--priority P2] --desc "..." — 任务一键移交
//   deploy-check <插件目录> [--runtime R] [--json] — 跨设备部署安全审查（依赖/API漂移/解析链）
//
// 设计原则：纯 std::net + serde_json（与 node-bridge/rust-blackboard 同风格），
// 零外部 HTTP 依赖，交叉编译零负担。本地模型摘要走 ollama HTTP API。

mod bb;
mod tailnet_proxy;
mod workflow;
mod bus_bridge;
mod deploy_check;
mod version;
mod repo;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const BB_DEFAULT: &str = "http://127.0.0.1:8792";
const OLLAMA: &str = "http://127.0.0.1:11434/api/generate";
const MODEL: &str = "qwen2.5:3b";

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

// ── 通用 HTTP 工具（纯 std::net）──
fn http_request(url: &str, method: &str, body: Option<&str>, timeout_secs: u64) -> Result<(u16, String), String> {
    let u = url.trim_start_matches("http://").trim_end_matches('/');
    let (hostport, path) = match u.find('/') {
        Some(i) => (&u[..i], &u[i..]),
        None => (u, "/"),
    };
    let (host, port) = match hostport.rfind(':') {
        Some(i) => (hostport[..i].to_string(), hostport[i+1..].parse().unwrap_or(80)),
        None => (hostport.to_string(), 80),
    };
    let mut stream = TcpStream::connect((host.as_str(), port))
        .map_err(|e| format!("connect {}:{} -> {}", host, port, e))?;
    stream.set_read_timeout(Some(Duration::from_secs(timeout_secs)))
        .map_err(|e| format!("set timeout: {}", e))?;
    stream.set_write_timeout(Some(Duration::from_secs(timeout_secs)))
        .map_err(|e| format!("set timeout: {}", e))?;

    let body = body.unwrap_or("");
    let req = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        method, path, hostport, body.len(), body
    );
    stream.write_all(req.as_bytes()).map_err(|e| format!("write: {}", e))?;
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).map_err(|e| format!("read: {}", e))?;
    let text = String::from_utf8_lossy(&resp).to_string();
    let status: u16 = text.lines().next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body_part = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    Ok((status, body_part))
}

fn bb_get(bb: &str, path: &str) -> Result<serde_json::Value, String> {
    let (st, body) = http_request(&format!("{}/{}", bb.trim_end_matches('/'), path.trim_start_matches('/')), "GET", None, 10)?;
    if st != 200 {
        return Err(format!("GET {} -> {}", path, st));
    }
    serde_json::from_str(&body).map_err(|e| format!("bad json: {}", e))
}

fn bb_put(bb: &str, path: &str, v: &serde_json::Value) -> Result<(u16, String), String> {
    http_request(&format!("{}/{}", bb.trim_end_matches('/'), path.trim_start_matches('/')), "PUT", Some(&v.to_string()), 10)
}

// ── 本地模型摘要（ollama，零 API 成本）──
fn summarize(text: &str) -> String {
    let prompt = format!(
        "这是黑板消息内容：{}。请用一行中文摘要（≤30字），只输出摘要本身。",
        text.chars().take(500).collect::<String>()
    );
    let body = serde_json::json!({"model": MODEL, "prompt": prompt, "stream": false});
    match http_request(OLLAMA, "POST", Some(&body.to_string()), 30) {
        Ok((st, resp)) => {
            if st == 200 {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                    return v.get("response").and_then(|r| r.as_str())
                        .unwrap_or("").trim().chars().take(60).collect::<String>();
                }
            }
            "[摘要失败]".to_string()
        }
        Err(e) => format!("[本地模型不可用: {}]", e.chars().take(40).collect::<String>()),
    }
}

fn extract_text(value: &serde_json::Value) -> String {
    if value.is_null() { return String::new(); }
    if let Some(s) = value.as_str() { return s.chars().take(500).collect(); }
    if let Some(arr) = value.as_array() {
        return arr.iter().map(|v| v.as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>().join(" ").chars().take(500).collect();
    }
    if let Some(obj) = value.as_object() {
        for k in ["content", "subject", "summary", "note"] {
            if let Some(v) = obj.get(k) {
                if let Some(arr) = v.as_array() {
                    return arr.iter().map(|x| x.as_str().unwrap_or("").to_string())
                        .collect::<Vec<_>>().join(" ").chars().take(500).collect();
                }
                if let Some(s) = v.as_str() {
                    return s.chars().take(500).collect();
                }
            }
        }
        return value.to_string().chars().take(500).collect();
    }
    value.to_string().chars().take(500).collect()
}

// ── 子命令 1: bb-read（看黑板摘要）──
fn cmd_bb_read(args: &[String]) {
    let mut key: Option<String> = None;
    let mut prefix: Option<String> = None;
    let mut last = 5;
    let mut full = false;
    let mut scan: Option<String> = None;
    let mut bb = BB_DEFAULT.to_string();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--full" => full = true,
            "--prefix" => { i += 1; if i < args.len() { prefix = Some(args[i].clone()); } }
            "--last" => { i += 1; if i < args.len() { last = args[i].parse().unwrap_or(5); } }
            "--scan" => { i += 1; if i < args.len() { scan = Some(args[i].clone()); } }
            "--bb" => { i += 1; if i < args.len() { bb = args[i].clone(); } }
            "--" => { i += 1; if i < args.len() { key = Some(args[i].clone()); } }
            s if s.starts_with('-') && s != "-" => {
                // 裸 key（无 -- 前缀，如 notes/mac-mini/x）
                if s.starts_with("notes/") || s.starts_with("data/") || s.starts_with("tasks/") {
                    key = Some(s.to_string());
                }
            }
            s if !s.starts_with('-') => {
                if key.is_none() && (s.starts_with("notes/") || s.starts_with("data/") || s.starts_with("tasks/")) {
                    key = Some(s.to_string());
                }
            }
            _ => {}
        }
        i += 1;
    }

    // scan 模式：读本地 jsonl
    if let Some(f) = scan {
        let content = std::fs::read_to_string(&f).unwrap_or_default();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        let start = lines.len().saturating_sub(last);
        for line in &lines[start..] {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                let k = v.get("key").and_then(|x| x.as_str()).unwrap_or("inbox");
                let ts = v.get("ts").and_then(|x| x.as_str()).unwrap_or("").chars().take(19).collect::<String>();
                let value = v.get("value").cloned().unwrap_or(serde_json::Value::Null);
                let text = extract_text(&value);
                if text.is_empty() {
                    println!("· {} [{}]: (空)", k, ts);
                    continue;
                }
                let summ = summarize(&text);
                println!("· {} [{}]: {}", k, ts, summ);
            }
        }
        return;
    }

    // prefix 模式：读本地订阅 inbox（已按前缀过滤，避免黑板全量 3.2MB）
    if let Some(p) = prefix {
        let inbox_dir = format!("{}/.dsh/inbox/bb", std::env::var("HOME").unwrap_or_else(|_| ".".into()));
        let mut entries: Vec<(String, String, serde_json::Value)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&inbox_dir) {
            for e in rd.flatten() {
                let fname = e.file_name().to_string_lossy().to_string();
                if !fname.ends_with(".jsonl") { continue; }
                let content = std::fs::read_to_string(e.path()).unwrap_or_default();
                for line in content.lines() {
                    if line.trim().is_empty() { continue; }
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                        let k = v.get("key").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        if k.starts_with(&p) {
                            let ts = v.get("ts").and_then(|x| x.as_str()).unwrap_or("").to_string();
                            let val = v.get("value").cloned().unwrap_or(serde_json::Value::Null);
                            entries.push((k, ts, val));
                        }
                    }
                }
            }
        }
        entries.sort_by(|a, b| b.1.cmp(&a.1));
        entries.truncate(last);
        if entries.is_empty() {
            println!("（本地订阅无 {} 前缀事件——检查 bb-sub-daemon 是否运行）", p);
            return;
        }
        for (k, ts, val) in entries {
            let text = extract_text(&val);
            if text.is_empty() { continue; }
            if full {
                println!("=== {} [{}] ===", k, ts.chars().take(19).collect::<String>());
                println!("{}", val.to_string().chars().take(800).collect::<String>());
                println!();
            } else {
                let summ = summarize(&text);
                println!("· {} [{}]: {}", k, ts.chars().take(19).collect::<String>(), summ);
            }
        }
        return;
    }

    // 单键模式
    if let Some(k) = key {
        match bb_get(&bb, &k) {
            Ok(d) => {
                let ts = d.get("ts").and_then(|x| x.as_str()).unwrap_or("").chars().take(19).collect::<String>();
                let value = d.get("value").cloned().unwrap_or(serde_json::Value::Null);
                if full {
                    println!("=== {} [{}] ===", k, ts);
                    println!("{}", value.to_string().chars().take(800).collect::<String>());
                } else {
                    let text = extract_text(&value);
                    let summ = if text.is_empty() { "(空)".to_string() } else { summarize(&text) };
                    println!("· {} [{}]: {}", k, ts, summ);
                }
            }
            Err(e) => println!("黑板错误: {}", e),
        }
        return;
    }

    println!("用法: dsh-tools bb-read <key> [--full] | --prefix P --last N | --scan FILE");
}

// ── 子命令 2: bb-sub（常驻订阅器）──
fn cmd_bb_sub(args: &[String]) {
    let mut agent = "all".to_string();
    let mut prefixes: Vec<String> = vec!["notes/collab/".to_string()];
    let mut sse = "http://127.0.0.1:8803/events".to_string();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--agent" => { i += 1; if i < args.len() { agent = args[i].clone(); } }
            "--prefixes" => {
                i += 1;
                if i < args.len() {
                    prefixes = args[i].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                }
            }
            "--sse" => { i += 1; if i < args.len() { sse = args[i].clone(); } }
            _ => {}
        }
        i += 1;
    }

    let outdir = format!("{}/.dsh/inbox/bb", std::env::var("HOME").unwrap_or_else(|_| ".".into()));
    std::fs::create_dir_all(&outdir).ok();
    let outfile = format!("{}/{}.jsonl", outdir, agent);

    println!("[bb-sub] {} agent={} prefixes={:?} -> {}", now(), agent, prefixes, outfile);
    println!("[bb-sub] SSE 连接中 {}", sse);

    // 简单长连接：HTTP GET 流式读（Connection: keep-alive 由服务端保持）
    let u = sse.trim_start_matches("http://").trim_end_matches('/');
    let (hostport, path) = match u.find('/') {
        Some(i) => (&u[..i], &u[i..]),
        None => (u, "/"),
    };
    let (host, port) = match hostport.rfind(':') {
        Some(i) => (hostport[..i].to_string(), hostport[i+1..].parse().unwrap_or(80)),
        None => (hostport.to_string(), 80),
    };

    let mut backoff = 2u64;
    loop {
        match TcpStream::connect((host.as_str(), port)) {
            Ok(mut stream) => {
                stream.set_read_timeout(Some(Duration::from_secs(60))).ok();
                let req = format!(
                    "GET {} HTTP/1.1\r\nHost: {}\r\nAccept: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
                    path, hostport
                );
                stream.write_all(req.as_bytes()).ok();
                println!("[bb-sub] {} SSE 已连接 {}", now(), sse);
                backoff = 2;

                // 流式读 SSE（逐行）
                let mut buf = [0u8; 4096];
                let mut line_buf = String::new();
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) => break, // 连接关闭
                        Ok(n) => {
                            let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                            for ch in chunk.chars() {
                                if ch == '\n' {
                                    let line = line_buf.trim().to_string();
                                    line_buf.clear();
                                    if let Some(data) = line.strip_prefix("data:") {
                                        if let Ok(evt) = serde_json::from_str::<serde_json::Value>(data.trim()) {
                                            let key = evt.get("key").and_then(|k| k.as_str()).unwrap_or("").to_string();
                                            if key.is_empty() { continue; }
                                            if !prefixes.iter().any(|p| key.starts_with(p)) { continue; }
                                            // 追加写 jsonl
                                            let entry = serde_json::json!({
                                                "ts": now(),
                                                "key": key,
                                                "value": evt.get("value").cloned().unwrap_or(serde_json::Value::Null),
                                                "version": evt.get("version").cloned().unwrap_or(serde_json::Value::Null),
                                            });
                                            let line = format!("{}\n", entry.to_string());
                                            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&outfile) {
                                                let _ = f.write_all(line.as_bytes());
                                            }
                                            println!("[bb-sub] {} [{}] 📩 {}", now(), agent, key);
                                        }
                                    }
                                } else {
                                    line_buf.push(ch);
                                }
                            }
                        }
                        Err(e) => {
                            if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut {
                                // 读超时（60s 无数据）：发 ping 保活？服务端 15s 有 ping，继续等
                                continue;
                            }
                            println!("[bb-sub] {} 读取错误: {}，重连", now(), e);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                println!("[bb-sub] {} 连接失败: {}，{}s 后重连", now(), e, backoff);
            }
        }
        std::thread::sleep(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(60);
    }
}

// ── 子命令 3: handoff（任务一键移交）──
fn cmd_handoff(args: &[String]) {
    let mut name = String::new();
    let mut files: Vec<String> = Vec::new();
    let mut to = "i9".to_string();
    let mut task_type = "general".to_string();
    let mut priority = "P2".to_string();
    let mut desc = String::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => { i += 1; if i < args.len() { name = args[i].clone(); } }
            "--files" => { i += 1; while i < args.len() && !args[i].starts_with("--") { files.push(args[i].clone()); i += 1; } continue; }
            "--to" => { i += 1; if i < args.len() { to = args[i].clone(); } }
            "--type" => { i += 1; if i < args.len() { task_type = args[i].clone(); } }
            "--priority" => { i += 1; if i < args.len() { priority = args[i].clone(); } }
            "--desc" => { i += 1; if i < args.len() { desc = args[i].clone(); } }
            _ => {}
        }
        i += 1;
    }

    if name.is_empty() {
        println!("用法: dsh-tools handoff --name X --files 'a b c' --to i9 --type ocr --priority P2 --desc '...'");
        return;
    }

    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let now_iso = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    // ASCII key（中文名 → task 兜底）
    let ascii: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    let key = if ascii.is_empty() { "task".to_string() } else { ascii.chars().take(20).collect() };

    println!("[handoff] {} 移交任务: {} → {} ({}/{})", now_iso, name, to, priority, task_type);

    // ① 打包（files → genebank shared tar.gz）
    let gb_shared = format!("{}/dsh-collab/datasets/shared", std::env::var("HOME").unwrap_or_default());
    std::fs::create_dir_all(&gb_shared).ok();
    let pkg_name = format!("{}-{}.tar.gz", name.replace(' ', "_"), ts);
    let pkg_path = format!("{}/{}", gb_shared, pkg_name);

    let mut pkg_url = String::new();
    if !files.is_empty() {
        // 简化打包：用 tar 命令（跨平台有 tar）
        let mut cmd = std::process::Command::new("tar");
        cmd.arg("-czf").arg(&pkg_path);
        for f in &files {
            cmd.arg(f);
        }
        match cmd.status() {
            Ok(st) if st.success() => {
                pkg_url = format!("http://100.120.203.20:8801/shared/{}", pkg_name);
                println!("  ① 打包: {} (URL: {})", pkg_path, pkg_url);
            }
            _ => println!("  ① 打包失败（文件不存在？）"),
        }
    } else {
        println!("  ① 无文件打包");
    }

    // ② 任务卡
    let task = serde_json::json!({
        "task_id": format!("t-{}-{}", key, ts),
        "from": "coordinator",
        "to": to,
        "type": task_type,
        "priority": priority,
        "subject": format!("{}（{}/{}）", name, priority, task_type),
        "content": [
            format!("【任务】{}", desc),
            if pkg_url.is_empty() { "【数据】无".to_string() } else { format!("【数据】{}", pkg_url) },
            format!("【排期】{} 后台。", priority),
            "【回报】完成后写黑板 notes/mac-mini/ + 结果放 genebank /shared/。".to_string(),
        ],
        "ts": now_iso,
    });
    let q_path = format!("tasks/{}/queue/{}-{}", to, ts, key);
    match bb_put(BB_DEFAULT, &q_path, &task) {
        Ok((st, _)) => println!("  ② 任务卡: /{} -> {}", q_path, st),
        Err(e) => println!("  ② 任务卡失败: {}", e),
    }

    // ③ 通知
    let note = serde_json::json!({
        "from": "coordinator",
        "to": to,
        "ts": now_iso,
        "subject": format!("📋 任务：{}（{}/{}）按你排期", name, priority, task_type),
        "content": task.get("content").cloned().unwrap_or(serde_json::Value::Null),
        "type": "task",
    });
    let n_path = format!("notes/{}/coordinator-task-{}", to, key);
    match bb_put(BB_DEFAULT, &n_path, &note) {
        Ok((st, _)) => println!("  ③ 通知: /{} -> {}", n_path, st),
        Err(e) => println!("  ③ 通知失败: {}", e),
    }

    // ④ 台账
    let ledger = serde_json::json!({
        "from": "coordinator",
        "ts": now_iso,
        "name": name,
        "to": to,
        "task_type": task_type,
        "priority": priority,
        "desc": desc,
        "package_url": pkg_url,
        "task_queue": q_path,
        "note_key": n_path,
        "status": "dispatched",
    });
    let h_path = format!("data/handoffs/{}-{}", ts, key);
    match bb_put(BB_DEFAULT, &h_path, &ledger) {
        Ok((st, _)) => println!("  ④ 台账: /{} -> {}", h_path, st),
        Err(e) => println!("  ④ 台账失败: {}", e),
    }

    println!("[handoff] ✅ 完成: {} → {} (task_id: t-{}-{})", name, to, key, ts);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("dsh-tools — 黑板固化工具集（Rust）");
        println!("子命令:");
        println!("  bb-read <key> [--full] | --prefix P --last N | --scan FILE");
        println!("  bb-sub --agent NAME --prefixes 'a,b,c'");
        println!("  handoff --name X --files 'a b' --to i9 --type ocr [--priority P2] --desc '...'");
        println!("  workflow <workflow.json>   # 串行/并行编排（token 预算门禁）");
        println!("  bus-bridge --port 8791 --queue ~/.dsh/bus-queue  # 跨设备总线桥");
        println!("  version --type fix|feat|breaking --desc '描述' [--repo 路径] [--dry-run]  # 自动化版本管理");
        println!("  repo setup/push/sync/status  # GitHub/Gitee 双仓与版本全链路");
        return;
    }
    match args[1].as_str() {
        "bb-read" => cmd_bb_read(&args[2..]),
        "bb-sub" => cmd_bb_sub(&args[2..]),
        "handoff" => cmd_handoff(&args[2..]),
        "bus-bridge" => cmd_bus_bridge(&args[2..]),
        "tailnet-proxy" => {
            // dsh-tools tailnet-proxy --host <ip> --port 3081
            let mut host = "0.0.0.0".to_string();
            let mut port = 3081u16;
            let mut i = 0;
            while i < args.len() {
                match args[i].as_str() {
                    "--host" => { i += 1; if i < args.len() { host = args[i].clone(); } }
                    "--port" => { i += 1; if i < args.len() { port = args[i].parse().unwrap_or(3081); } }
                    _ => {}
                }
                i += 1;
            }
            tailnet_proxy::run(&host, port);
            return;
        }
        "assess" => cmd_assess(&args[2..]),
        "deploy-check" => {
            let code = deploy_check::run(&args[2..]);
            std::process::exit(code);
        }
        "agent-msg" => cmd_agent_msg(&args[2..]),
        "agent-thread" => cmd_agent_thread(&args[2..]),
        "workflow" => {
            if args.len() < 2 { println!("用法: dsh-tools workflow <workflow.json>"); return; }
            let (ok, _) = workflow::run_workflow(&args[2]);
            std::process::exit(if ok { 0 } else { 1 });
        }
        "version" => {
            std::process::exit(version::run(&args[2..]));
        }
        "repo" => {
            std::process::exit(repo::run(&args[2..]));
        }
        other => println!("未知子命令: {}", other),
    }
}


// ── 子命令 4: bus-bridge（跨设备总线桥，替代 bus-bridge.js）──
fn cmd_bus_bridge(args: &[String]) {
    let mut port: u16 = 8791;
    let mut host = "0.0.0.0".to_string();
    let mut token = String::new();
    let mut queue_dir = std::env::var("HOME").unwrap_or_default() + "/.dsh/bus-queue";
    let mut allowed_from: Vec<String> = Vec::new();
    let mut allowed_actions: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" => { i += 1; if i < args.len() { port = args[i].parse().unwrap_or(8791); } }
            "--host" => { i += 1; if i < args.len() { host = args[i].clone(); } }
            "--token" => { i += 1; if i < args.len() { token = args[i].clone(); } }
            "--queue" => { i += 1; if i < args.len() { queue_dir = args[i].clone(); } }
            "--allowed-from" => { i += 1; if i < args.len() { allowed_from = args[i].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(); } }
            "--allowed-actions" => { i += 1; if i < args.len() { allowed_actions = args[i].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(); } }
            _ => {}
        }
        i += 1;
    }
    bus_bridge::run(port, &host, &token, &queue_dir, &allowed_from, &allowed_actions);
}


// ── 子命令: agent-msg / agent-thread（跨设备 agent_send 变体）──
// 基于黑板 notes/{node}/agent-msg-* 通道的消息对话：
//   agent-msg --to i9 --text "..." [--thread x-xxx] [--reply-to agent-msg-xxx]
//   agent-thread --thread x-xxx    # 读线程（聚合该 thread 的全部消息）
// 语义对齐 agent_send：线程延续 / 去重（同from+thread+text 10min skip）/ 回复链
// 跨设备：写目标节点黑板 → node-bridge 消费落盘 → 目标智能体感知

fn agent_msg_key() -> String {
    format!("agent-msg-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"))
}

fn cmd_agent_msg(args: &[String]) {
    let mut to = String::new();
    let mut text = String::new();
    let mut thread: Option<String> = None;
    let mut reply_to: Option<String> = None;
    let mut from_agent: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--to" => { i += 1; if i < args.len() { to = args[i].clone(); } }
            "--text" => { i += 1; if i < args.len() { text = args[i].clone(); } }
            "--thread" => { i += 1; if i < args.len() { thread = Some(args[i].clone()); } }
            "--reply-to" => { i += 1; if i < args.len() { reply_to = Some(args[i].clone()); } }
            "--from-agent" => { i += 1; if i < args.len() { from_agent = Some(args[i].clone()); } }
            _ => {}
        }
        i += 1;
    }

    if to.is_empty() || text.is_empty() {
        println!("用法: dsh-tools agent-msg --to i9|mbp|mac-mini --text \"消息\" [--thread x-xxx] [--reply-to agent-msg-xxx]");
        return;
    }

    // ── v2.3 门禁：跨设备消息同样遵守最短沟通规则 ──
    // 全文（>200字）且非 urgent → 拒绝发送，提示写黑板
    let has_urgent = text.to_lowercase().contains("urgent");
    let has_bb_ref = text.contains("看黑板");
    if text.chars().count() > 200 && !has_urgent && !has_bb_ref {
        println!("[agent-msg] ⚠️ 门禁：消息 {} 字 > 200 且非紧急（v2.3 最短沟通规则）", text.chars().count());
        println!("  跨设备消息本来就写黑板，请用『短提醒 + 内容写 data/ 或 notes/』模式：");
        println!("  dsh-tools agent-msg --to {} --text '看黑板 data/<key>（一句话提示）'", to);
        println!("  或先写内容到黑板（data/<域>/<key>），再发短提醒。");
        return;
    }

    let node = std::env::var("DSH_NODE_ID").unwrap_or_else(|_| "mac-mini".to_string());
    let key = agent_msg_key();
    // 线程：无则用内容 hash（同主题延续）
    let thread_id = thread.unwrap_or_else(|| {
        format!("x-{:x}", text.chars().take(20).fold(0u64, |acc, c| acc.wrapping_add(c as u64)))
    });

    let msg = serde_json::json!({
        "from": node,
        "from_agent": from_agent.unwrap_or_else(|| "dsh-tools".to_string()),
        "to": to,
        "thread": thread_id,
        "text": text,
        "seq": chrono::Local::now().timestamp_millis(),
        "reply_to": reply_to.unwrap_or_default(),
        "ts": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
        "type": "agent-msg",
    });

    let path = format!("notes/{}/{}", to, key);
    match bb_put(BB_DEFAULT, &path, &msg) {
        Ok((st, _)) => {
            println!("[agent-msg] {} → {} ({}): {}", node, to, key, text.chars().take(40).collect::<String>());
            println!("  thread: {}", thread_id);
            if st != 200 { println!("  ⚠️ 黑板返回 {}", st); }
            // 同步线程索引（跨设备可读聚合）
            append_thread_index(&thread_id, &msg);
        }
        Err(e) => println!("[agent-msg] 发送失败: {}", e),
    }
}

/// 线程索引：data/threads/<thread>.json 追加消息（跨设备聚合，agent-thread 读它）
fn append_thread_index(thread_id: &str, msg: &serde_json::Value) {
    let safe = thread_id.replace(|c: char| !c.is_alphanumeric() && c != '-', "_");
    let path = format!("data/threads/{}", safe);
    // 读现有索引（若有）
    let mut entries: Vec<serde_json::Value> = Vec::new();
    if let Ok(d) = bb_get(BB_DEFAULT, &path) {
        let val = d.get("value").cloned().unwrap_or(serde_json::Value::Null);
        if let Some(arr) = val.get("messages").and_then(|x| x.as_array()) {
            entries = arr.clone();
        }
    }
    entries.push(msg.clone());
    let idx = serde_json::json!({
        "thread": thread_id,
        "messages": entries,
        "updated": chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
    });
    let _ = bb_put(BB_DEFAULT, &path, &idx);
}

fn cmd_agent_thread(args: &[String]) {
    let mut thread_id = String::new();
    let mut node: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--thread" => { i += 1; if i < args.len() { thread_id = args[i].clone(); } }
            "--node" => { i += 1; if i < args.len() { node = Some(args[i].clone()); } }
            _ => {}
        }
        i += 1;
    }
    if thread_id.is_empty() {
        println!("用法: dsh-tools agent-thread --thread x-xxx [--node i9]");
        return;
    }

    // 读线程索引 data/threads/<thread>.json（跨设备聚合）
    let safe = thread_id.replace(|c: char| !c.is_alphanumeric() && c != '-', "_");
    match bb_get(BB_DEFAULT, &format!("data/threads/{}", safe)) {
        Ok(d) => {
            let val = d.get("value").cloned().unwrap_or(serde_json::Value::Null);
            let msgs = val.get("messages").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            if msgs.is_empty() {
                println!("线程 {} 无消息（未发过 agent-msg）", thread_id);
                return;
            }
            println!("线程 {} （{} 条）:", thread_id, msgs.len());
            for m in &msgs {
                let from = m.get("from").and_then(|x| x.as_str()).unwrap_or("?");
                let ts = m.get("ts").and_then(|x| x.as_str()).unwrap_or("").chars().take(19).collect::<String>();
                let text = m.get("text").and_then(|x| x.as_str()).unwrap_or("").chars().take(100).collect::<String>();
                println!("· [{}] {}: {}", ts, from, text);
            }
        }
        Err(e) => println!("线程 {} 读取失败: {}", thread_id, e),
    }
}



// ── 子命令: assess（工具评估器：语言迁移 + 变体泛化建议）──
// Q2: 评估组件适合 Rust/Go/保留（依赖扫描+复杂度+资源需求）
// Q3: 评估工具瓶颈→泛化方向建议（跨设备/多租户/批量化/订阅化）
// 输入: --tool <名> --src <路径>（或 --bottleneck <描述>）
// 输出: 推荐语言 + 迁移难度 + 预期收益 / 泛化建议

fn cmd_assess(args: &[String]) {
    let mut tool = String::new();
    let mut src = String::new();
    let mut bottleneck = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--tool" => { i += 1; if i < args.len() { tool = args[i].clone(); } }
            "--src" => { i += 1; if i < args.len() { src = args[i].clone(); } }
            "--bottleneck" => { i += 1; if i < args.len() { bottleneck = args[i].clone(); } }
            _ => {}
        }
        i += 1;
    }

    if !src.is_empty() {
        // 语言迁移评估（Q2）
        println!("══ 语言迁移评估 ══");
        let content = std::fs::read_to_string(&src).unwrap_or_default();
        let lines = content.lines().count();
        println!("工具: {}  | 源码: {}  | {} 行", if tool.is_empty() { "?" } else { &tool }, src, lines);

        // ① 依赖扫描（SDK 锁定判断）
        let has_sdk = ["@modelcontextprotocol", "@wecom", "better-sqlite3", "playwright", "puppeteer"].iter()
            .any(|s| content.contains(s));
        let has_http = content.contains("http.createServer") || content.contains("http.request");
        let has_sse = content.contains("text/event-stream") || content.contains("/events");
        let has_sqlite = content.contains("sqlite") || content.contains("better-sqlite3");
        println!("依赖分析: SDK锁定={} HTTP={} SSE={} SQLite={}", has_sdk, has_http, has_sse, has_sqlite);

        // ② 复杂度
        let complexity = if lines > 400 { "高" } else if lines > 150 { "中" } else { "低" };
        println!("复杂度: {}（{} 行）", complexity, lines);

        // ③ 决策
        if has_sdk {
            println!("→ 推荐: 保留原语言（官方 SDK 锁定，Rust 化成本>收益）");
            println!("   Rust化难度: 高 | 收益: 低（SDK 无法替代）");
        } else if has_sqlite && !has_sdk {
            println!("→ 推荐: Rust（纯逻辑+SQLite，rusqlite 可替代；或 Go database/sql）");
            println!("   Rust化难度: 中 | 收益: 中（内存省50%+零依赖分发）");
        } else if has_sse || has_http {
            println!("→ 推荐: Rust（HTTP/SSE 服务，与 dsh-tools 同栈）");
            println!("   Rust化难度: 低-中 | 收益: 高（常驻内存 5.97MB vs Node ~50MB）");
        } else {
            println!("→ 推荐: Rust（纯逻辑工具，直接并入 dsh-tools）");
            println!("   Rust化难度: 低 | 收益: 高（单二进制零依赖）");
        }
        // 预期收益估算
        println!("预期收益: 内存 -50~90% | 启动快 2-3x | 分发零依赖（对照 LiteLLM 基准 15x/11x）");
        return;
    }

    if !bottleneck.is_empty() {
        // 工具泛化评估（Q3）
        println!("══ 工具泛化评估 ══");
        println!("工具瓶颈: {}", bottleneck);
        let b = bottleneck.to_lowercase();
        let mut dims: Vec<&str> = Vec::new();
        if b.contains("跨设备") || b.contains("跨机") || b.contains("remote") {
            dims.push("跨设备（黑板 notes 通道 → agent-msg 模式）");
        }
        if b.contains("多租户") || b.contains("多用户") {
            dims.push("多租户（按 agent/设备隔离命名空间）");
        }
        if b.contains("批量") || b.contains("大量") {
            dims.push("批量化（workflow 串行/并行编排）");
        }
        if b.contains("异步") || b.contains("实时") {
            dims.push("异步化（bb-sub 订阅器事件驱动）");
        }
        if b.contains("订阅") || b.contains("推送") {
            dims.push("订阅化（SSE 事件桥 8803）");
        }
        if dims.is_empty() {
            dims.push("通用化（抽象输入输出，复用黑板 KV）");
        }
        println!("泛化方向建议:");
        for d in dims { println!("  · {}", d); }
        println!("参考: agent_send(同宿主) → 瓶颈:不跨设备 → 变体 agent-msg(黑板通道) ✅ 已实现");
        return;
    }

    println!("用法: dsh-tools assess --tool <名> --src <路径>   # 语言迁移评估（Q2）");
    println!("      dsh-tools assess --bottleneck <描述>       # 工具泛化评估（Q3）");
}

