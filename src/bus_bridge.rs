// bus_bridge.rs — 跨设备总线桥（Rust 版，替代 bus-bridge.js）
// 功能等价：POST /bus/send(?wait=1) · GET /bus/receive(?target=) · POST /bus/reply
//           GET /bus/outbox(?task_id=&from=&since=&consume=) · GET /bus/status · GET /health
// 队列：文件 JSON（~/.dsh/bus-queue/tasks|outbox/），状态机 queued→processing→done|failed
// 设计：纯 std::net + serde_json（与 dsh-tools 其他子命令同栈），零外部依赖
// 安全：X-Webhook-Token 可选鉴权 + 来源/动作白名单 + TTL 过期

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::Duration;

const TTL_DEFAULT: u64 = 3600;
const WAIT_TIMEOUT_MS: u64 = 30_000;

struct Config {
    port: u16,
    host: String,
    token: String,
    queue_dir: PathBuf,
    allowed_from: Vec<String>,
    allowed_actions: Vec<String>,
}

fn tasks_dir(cfg: &Config) -> PathBuf { cfg.queue_dir.join("tasks") }
fn outbox_dir(cfg: &Config) -> PathBuf { cfg.queue_dir.join("outbox") }

fn ensure_dirs(cfg: &Config) {
    std::fs::create_dir_all(tasks_dir(cfg)).ok();
    std::fs::create_dir_all(outbox_dir(cfg)).ok();
}

fn now_iso() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn task_path(cfg: &Config, task_id: &str) -> PathBuf {
    tasks_dir(cfg).join(format!("{}.json", task_id))
}

fn outbox_path(cfg: &Config, task_id: &str) -> PathBuf {
    outbox_dir(cfg).join(format!("{}.json", task_id))
}

fn atomic_write(path: &std::path::Path, v: &serde_json::Value) {
    let tmp = path.with_extension("json.tmp");
    if let Ok(s) = serde_json::to_string_pretty(v) {
        let _ = std::fs::write(&tmp, s);
        let _ = std::fs::rename(&tmp, path);
    }
}

fn read_json(path: &std::path::Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok())
}

fn list_files(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.ends_with(".json") { out.push(n); }
        }
    }
    out.sort();
    out
}

fn gen_task_id() -> String {
    // 简化 UUID v4（纯 std 无 uuid crate）
    let now = now_ms();
    format!("task-{:x}-{:x}", now, std::process::id())
}

// TTL 懒清理
fn sweep_expired(cfg: &Config) {
    let now = now_ms();
    for f in list_files(&tasks_dir(cfg)) {
        let tid = f.trim_end_matches(".json").to_string();
        let p = task_path(cfg, &tid);
        if let Some(mut task) = read_json(&p) {
            let st = task.get("status").and_then(|x| x.as_str()).unwrap_or("");
            if st == "queued" {
                let ttl = task.get("ttl_sec").and_then(|x| x.as_u64()).unwrap_or(TTL_DEFAULT);
                let created = task.get("created_at").and_then(|x| x.as_str()).unwrap_or("");
                if let Ok(ct) = parse_iso_ms(created) {
                    if now > ct + ttl * 1000 {
                        task["status"] = serde_json::json!("failed");
                        task["error"] = serde_json::json!("TTL expired");
                        task["finished_at"] = serde_json::json!(now_iso());
                        atomic_write(&p, &task);
                        let out = serde_json::json!({
                            "task_id": tid, "from": task.get("from").cloned().unwrap_or(serde_json::Value::Null),
                            "to": task.get("target").cloned().unwrap_or(serde_json::Value::Null),
                            "ok": false, "result": serde_json::Value::Null, "error": "TTL expired",
                            "finished_at": task["finished_at"].clone(),
                        });
                        atomic_write(&outbox_path(cfg, &tid), &out);
                    }
                }
            }
        }
    }
}

// 简化 ISO 解析：2026-08-27T01:23:45.123Z → epoch ms（仅取到秒精度）
fn parse_iso_ms(s: &str) -> Result<u64, String> {
    let s = s.replace("T", " ").replace("Z", "");
    let s = s.split('.').next().unwrap_or(&s);
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 2 { return Err("bad date".into()); }
    let d: Vec<&str> = parts[0].split('-').collect();
    let t: Vec<&str> = parts[1].split(':').collect();
    if d.len() < 3 || t.len() < 3 { return Err("bad date".into()); }
    let y: i64 = d[0].parse().map_err(|_| "year")?;
    let mo: i64 = d[1].parse().map_err(|_| "month")?;
    let day: i64 = d[2].parse().map_err(|_| "day")?;
    let h: i64 = t[0].parse().map_err(|_| "hour")?;
    let mi: i64 = t[1].parse().map_err(|_| "min")?;
    let se: i64 = t[2].parse().map_err(|_| "sec")?;
    // 简化：用 chrono 解析
    if let Some(dt) = chrono::NaiveDate::from_ymd_opt(y as i32, mo as u32, day as u32) {
        if let Some(tm) = chrono::NaiveTime::from_hms_opt(h as u32, mi as u32, se as u32) {
            let ndt = chrono::NaiveDateTime::new(dt, tm);
            return Ok(ndt.and_utc().timestamp_millis() as u64);
        }
    }
    Err("bad date".into())
}

// ---------- 业务 ----------

fn bus_send(cfg: &Config, body: &serde_json::Value) -> serde_json::Value {
    let from = body.get("from").and_then(|x| x.as_str()).unwrap_or("");
    let action = body.get("action").and_then(|x| x.as_str()).unwrap_or("");
    if from.is_empty() || action.is_empty() {
        return serde_json::json!({"ok": false, "errmsg": "from 与 action 必填"});
    }
    if !cfg.allowed_from.is_empty() && !cfg.allowed_from.iter().any(|a| a == from) {
        return serde_json::json!({"ok": false, "errmsg": format!("来源 {} 不在白名单", from)});
    }
    if !cfg.allowed_actions.is_empty() && !cfg.allowed_actions.iter().any(|a| a == action) {
        return serde_json::json!({"ok": false, "errmsg": format!("动作 {} 不在白名单", action)});
    }
    let tid = gen_task_id();
    let ttl = body.get("ttl_sec").and_then(|x| x.as_u64()).unwrap_or(TTL_DEFAULT);
    let task = serde_json::json!({
        "task_id": tid,
        "from": from,
        "target": body.get("target").and_then(|x| x.as_str()).unwrap_or("any"),
        "action": action,
        "payload": body.get("payload").cloned().unwrap_or(serde_json::Value::Null),
        "ttl_sec": ttl,
        "status": "queued",
        "created_at": now_iso(),
        "started_at": serde_json::Value::Null,
        "finished_at": serde_json::Value::Null,
        "reply": serde_json::Value::Null,
    });
    atomic_write(&task_path(cfg, &tid), &task);
    serde_json::json!({"ok": true, "task_id": tid, "status": "queued"})
}

fn bus_receive(cfg: &Config, target: &str) -> serde_json::Value {
    sweep_expired(cfg);
    for f in list_files(&tasks_dir(cfg)) {
        let tid = f.trim_end_matches(".json").to_string();
        let p = task_path(cfg, &tid);
        if let Some(mut task) = read_json(&p) {
            let st = task.get("status").and_then(|x| x.as_str()).unwrap_or("");
            if st != "queued" { continue; }
            let tgt = task.get("target").and_then(|x| x.as_str()).unwrap_or("any").to_string();
            if !target.is_empty() && tgt != target && tgt != "any" { continue; }
            task["status"] = serde_json::json!("processing");
            task["started_at"] = serde_json::json!(now_iso());
            atomic_write(&p, &task);
            return serde_json::json!({"ok": true, "task": {
                "task_id": tid, "from": task["from"].clone(), "target": task["target"].clone(),
                "action": task["action"].clone(), "payload": task["payload"].clone(),
                "created_at": task["created_at"].clone(),
            }});
        }
    }
    serde_json::json!({"ok": true, "task": serde_json::Value::Null})
}

fn bus_reply(cfg: &Config, body: &serde_json::Value) -> serde_json::Value {
    let tid = body.get("task_id").and_then(|x| x.as_str()).unwrap_or("");
    if tid.is_empty() { return serde_json::json!({"ok": false, "errmsg": "task_id 必填"}); }
    let p = task_path(cfg, tid);
    let mut task = match read_json(&p) {
        Some(t) => t,
        None => return serde_json::json!({"ok": false, "errmsg": format!("任务 {} 不存在", tid)}),
    };
    let st = task.get("status").and_then(|x| x.as_str()).unwrap_or("");
    if st != "processing" && st != "queued" {
        return serde_json::json!({"ok": false, "errmsg": format!("任务状态 {} 不可回复", st)});
    }
    let ok = body.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
    task["status"] = serde_json::json!(if ok { "done" } else { "failed" });
    task["finished_at"] = serde_json::json!(now_iso());
    task["reply"] = serde_json::json!({
        "ok": ok,
        "result": body.get("result").cloned().unwrap_or(serde_json::Value::Null),
        "error": body.get("error").cloned().unwrap_or(serde_json::Value::Null),
    });
    atomic_write(&p, &task);
    let out = serde_json::json!({
        "task_id": tid, "from": task["target"].clone(), "to": task["from"].clone(), "ok": ok,
        "result": body.get("result").cloned().unwrap_or(serde_json::Value::Null),
        "error": body.get("error").cloned().unwrap_or(serde_json::Value::Null),
        "finished_at": task["finished_at"].clone(),
    });
    atomic_write(&outbox_path(cfg, tid), &out);
    serde_json::json!({"ok": true, "task_id": tid, "status": task["status"].clone()})
}

fn bus_outbox(cfg: &Config, q: &std::collections::HashMap<String, String>) -> serde_json::Value {
    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut to_delete: Vec<PathBuf> = Vec::new();
    for f in list_files(&outbox_dir(cfg)) {
        let tid = f.trim_end_matches(".json").to_string();
        if let Some(by_id) = q.get("task_id") { if !by_id.is_empty() && *by_id != tid { continue; } }
        let p = outbox_path(cfg, &tid);
        if let Some(entry) = read_json(&p) {
            if let Some(frm) = q.get("from") {
                let to = entry.get("to").and_then(|x| x.as_str()).unwrap_or("");
                if !frm.is_empty() && frm != to { continue; }
            }
            if let Some(since) = q.get("since") {
                let fin = entry.get("finished_at").and_then(|x| x.as_str()).unwrap_or("");
                if !since.is_empty() && !fin.is_empty() {
                    if let (Ok(s_ms), Ok(f_ms)) = (parse_iso_ms(since), parse_iso_ms(fin)) {
                        if f_ms < s_ms { continue; }
                    }
                }
            }
            results.push(entry);
            let consume = q.get("consume").map(|s| s == "1" || s == "true").unwrap_or(false);
            if consume { to_delete.push(p); }
        }
    }
    let consumed = to_delete.len();
    for p in to_delete { let _ = std::fs::remove_file(p); }
    serde_json::json!({"ok": true, "results": results, "consumed": consumed})
}

fn bus_status(cfg: &Config) -> serde_json::Value {
    sweep_expired(cfg);
    let mut stats = serde_json::Map::new();
    for st in ["queued", "processing", "done", "failed"] {
        stats.insert(st.to_string(), serde_json::json!(0));
    }
    let mut recent: Vec<serde_json::Value> = Vec::new();
    let files = list_files(&tasks_dir(cfg));
    for f in files.iter().rev().take(10) {
        let tid = f.trim_end_matches(".json").to_string();
        if let Some(task) = read_json(&task_path(cfg, &tid)) {
            let st = task.get("status").and_then(|x| x.as_str()).unwrap_or("");
            if let Some(n) = stats.get_mut(st) { *n = serde_json::json!(n.as_u64().unwrap_or(0) + 1); }
            recent.push(serde_json::json!({
                "task_id": tid, "from": task.get("from").cloned().unwrap_or(serde_json::Value::Null),
                "target": task.get("target").cloned().unwrap_or(serde_json::Value::Null),
                "action": task.get("action").cloned().unwrap_or(serde_json::Value::Null),
                "status": st, "created_at": task.get("created_at").cloned().unwrap_or(serde_json::Value::Null),
            }));
        }
    }
    serde_json::json!({"ok": true, "stats": serde_json::Value::Object(stats),
        "outbox_count": list_files(&outbox_dir(cfg)).len(), "recent": recent})
}

// ---------- HTTP ----------

fn handle_conn(cfg: &Config, mut stream: std::net::TcpStream) {
    stream.set_read_timeout(Some(Duration::from_secs(35))).ok();
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                // 检测请求头结束
                if buf.windows(4).any(|w| w == b"\r\n\r\n") { break; }
                if buf.len() > 1_200_000 { break; }
            }
            Err(_) => break,
        }
    }
    let req_text = String::from_utf8_lossy(&buf).to_string();
    // 解析请求行
    let first_line = req_text.lines().next().unwrap_or("").to_string();
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 { return; }
    let method = parts[0].to_string();
    let target = parts[1].to_string();
    // 分离 path/query
    let (path, query_str) = match target.find('?') {
        Some(i) => (target[..i].to_string(), target[i+1..].to_string()),
        None => (target.clone(), String::new()),
    };
    // 解析 query
    let mut query: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for kv in query_str.split('&').filter(|s| !s.is_empty()) {
        if let Some((k, v)) = kv.split_once('=') {
            query.insert(k.to_string(), v.to_string());
        }
    }
    // body（POST）
    let body_part = req_text.split("\r\n\r\n").nth(1).unwrap_or("");
    let body: serde_json::Value = if body_part.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(body_part).unwrap_or(serde_json::json!({}))
    };
    // 鉴权
    if !cfg.token.is_empty() {
        let has_token = req_text.lines().any(|l| {
            l.to_lowercase().starts_with("x-webhook-token:") && l.contains(&cfg.token)
        });
        if !has_token {
            let _ = send_json(stream, 401, &serde_json::json!({"ok": false, "errmsg": "unauthorized"}));
            return;
        }
    }

    let resp = match (method.as_str(), path.as_str()) {
        ("GET", "/health") => serde_json::json!({"ok": true, "service": "bus-bridge", "ts": now_ms(), "queue": cfg.queue_dir.to_string_lossy()}),
        ("GET", "/bus/status") => bus_status(cfg),
        ("GET", "/bus/receive") => {
            let target = query.get("target").cloned().unwrap_or_default();
            bus_receive(cfg, &target)
        }
        ("GET", "/bus/outbox") => bus_outbox(cfg, &query),
        ("POST", "/bus/send") => {
            let wait = query.get("wait").map(|s| s == "1" || s == "true").unwrap_or(false);
            let r = bus_send(cfg, &body);
            if !wait || !r.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
                r
            } else {
                // 长轮询 ≤30s
                let tid = r.get("task_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let deadline = now_ms() + WAIT_TIMEOUT_MS;
                let mut result = serde_json::json!({"ok": true, "task_id": tid, "status": "queued", "note": "polling"});
                while now_ms() < deadline {
                    std::thread::sleep(Duration::from_millis(1000));
                    if let Some(task) = read_json(&task_path(cfg, &tid)) {
                        let st = task.get("status").and_then(|x| x.as_str()).unwrap_or("");
                        if st == "done" || st == "failed" {
                            let out = read_json(&outbox_path(cfg, &tid)).unwrap_or(serde_json::json!({}));
                            result = serde_json::json!({
                                "ok": true, "task_id": tid, "status": st,
                                "result": out.get("result").cloned().unwrap_or(serde_json::Value::Null),
                                "error": out.get("error").cloned().unwrap_or(serde_json::Value::Null),
                            });
                            break;
                        }
                    }
                }
                if result.get("status").and_then(|x| x.as_str()) == Some("queued") {
                    result["note"] = serde_json::json!(format!("等待超时（{}s），请 GET /bus/outbox?task_id={} 拉取", WAIT_TIMEOUT_MS/1000, tid));
                }
                result
            }
        }
        ("POST", "/bus/reply") => bus_reply(cfg, &body),
        _ => serde_json::json!({"ok": false, "errmsg": "not found"}),
    };
    let code = if resp.get("errmsg").is_some() && path == "/bus/send" { 200 } else { 200 };
    let _ = send_json(stream, code, &resp);
}

fn send_json(mut stream: std::net::TcpStream, code: u16, v: &serde_json::Value) -> std::io::Result<()> {
    let body = v.to_string();
    let reason = if code == 200 { "OK" } else if code == 401 { "Unauthorized" } else { "Not Found" };
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        code, reason, body.len(), body
    );
    stream.write_all(resp.as_bytes())
}

pub fn run(port: u16, host: &str, token: &str, queue_dir: &str, allowed_from: &[String], allowed_actions: &[String]) {
    let cfg = Config {
        port, host: host.to_string(), token: token.to_string(),
        queue_dir: PathBuf::from(queue_dir),
        allowed_from: allowed_from.to_vec(), allowed_actions: allowed_actions.to_vec(),
    };
    ensure_dirs(&cfg);
    let listener = match TcpListener::bind((cfg.host.as_str(), cfg.port)) {
        Ok(l) => l,
        Err(e) => { eprintln!("[bus-bridge] bind {}:{} failed: {}", cfg.host, cfg.port, e); return; }
    };
    println!("[bus-bridge] listening on http://{}:{}", cfg.host, cfg.port);
    println!("[bus-bridge] queue: {}", cfg.queue_dir.display());
    println!("[bus-bridge] auth: {}", if cfg.token.is_empty() { "disabled" } else { "X-Webhook-Token enabled" });
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let cfg = Config { port: cfg.port, host: cfg.host.clone(), token: cfg.token.clone(),
                    queue_dir: cfg.queue_dir.clone(), allowed_from: cfg.allowed_from.clone(), allowed_actions: cfg.allowed_actions.clone() };
                std::thread::spawn(move || handle_conn(&cfg, s));
            }
            Err(_) => {}
        }
    }
}
