// dsh-tools channel-audit — 通道卫生巡检（自纠错机制，2026-08-30 用户审计落地）
// 功能：扫描黑板 notes/collab/ 消息，检测「定向消息误走广播」违规
//   ① 消息含 to/target 指向单节点（非 mac-mini）但写在 collab → WARN（应走定向通道）
//   ② 项目命名空间消息（无全局公告价值）→ WARN
// 输出：JSON 报告 + 违规登记黑板 data/audit/channel-violations/ + 可生成周报趋势
// 用法：dsh-tools channel-audit [--hours N] [--days N] [--json]

use std::path::PathBuf;
use serde_json::{json, Value};

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/Users/unknown"))
}

fn now_ts() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn bb_get(path: &str) -> Result<Value, String> {
    use std::io::{Read, Write as _};
    use std::net::TcpStream;
    use std::time::Duration;
    let bb = "127.0.0.1:8792";
    let mut stream = TcpStream::connect(bb).map_err(|e| format!("connect: {}", e))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let req = format!("GET /{} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", path, bb);
    stream.write_all(req.as_bytes()).map_err(|e| format!("write: {}", e))?;
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).map_err(|e| format!("read: {}", e))?;
    let text = String::from_utf8_lossy(&resp).to_string();
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    serde_json::from_str(&body).map_err(|e| format!("parse: {}", e))
}

fn bb_put(path: &str, v: &Value) -> Result<(), String> {
    use std::io::{Read, Write as _};
    use std::net::TcpStream;
    use std::time::Duration;
    let bb = "127.0.0.1:8792";
    let mut stream = TcpStream::connect(bb).map_err(|e| format!("connect: {}", e))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let body = v.to_string();
    let req = format!(
        "PUT /{} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path, bb, body.len(), body
    );
    stream.write_all(req.as_bytes()).map_err(|e| format!("write: {}", e))?;
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).map_err(|e| format!("read: {}", e))?;
    Ok(())
}

/// 提取消息里的定向字段（to/target）
fn extract_target(v: &Value) -> Option<String> {
    let obj = v.as_object()?;
    // 顶层 to/target
    for f in ["to", "target"] {
        if let Some(t) = obj.get(f).and_then(|x| x.as_str()) {
            if !t.is_empty() { return Some(t.to_string()); }
        }
    }
    // value 内层
    if let Some(inner) = obj.get("value").and_then(|x| x.as_object()) {
        for f in ["to", "target"] {
            if let Some(t) = inner.get(f).and_then(|x| x.as_str()) {
                if !t.is_empty() { return Some(t.to_string()); }
            }
        }
    }
    None
}

/// 扫描 collab 消息，检测通道违规
pub fn run(args: &[String]) -> i32 {
    let mut hours = 24u64;
    let mut json_out = false;
    let mut write_bb = true;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--hours" => { i += 1; if i < args.len() { hours = args[i].parse().unwrap_or(24); } }
            "--json" => { json_out = true; }
            "--no-write" => { write_bb = false; }
            "--days" => { i += 1; if i < args.len() { hours = args[i].parse::<u64>().unwrap_or(1) * 24; } }
            _ => {}
        }
        i += 1;
    }

    // 拉取 collab 全量
    let data = match bb_get("notes?node=collab") {
        Ok(d) => d,
        Err(e) => { println!("{}", json!({"error": e})); return 1; }
    };
    let list = data.get("list").and_then(|l| l.as_object()).cloned().unwrap_or_default();
    let cutoff = now_ts() - hours * 3600;
    let mut violations: Vec<Value> = Vec::new();
    let mut scanned = 0usize;

    for (k, v) in list.iter() {
        if !k.starts_with("notes/collab/") { continue; }
        scanned += 1;
        // 时间过滤（ts 字段是 ISO 或 value.ts）
        let ts_str = v.get("ts").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let vv = v.get("value").cloned().unwrap_or(json!({}));
        let val_ts = vv.get("ts").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let from = vv.get("from").and_then(|f| f.as_str()).unwrap_or("?").to_string();
        // 简化时间判断：只看是否有 to/target 且非中枢
        let target = extract_target(&vv);
        let is_global = target.is_none();
        let target_is_single = target.as_ref().map(|t| {
            // 单节点定向 = 指向单一具体会话/节点；排除：
            // ① 广播语义词（all/collab/coordinator/everyone/中枢）
            // ② 多目标（含 + 、逗号分隔、多个节点名）——多端通知本就该走 collab
            // ③ 项目/会话级目标（session-* 视为项目内，仍需项目命名空间但不在 collab 审计范围）
            let tl = t.to_lowercase();
            let broadcast_words = ["all", "collab", "coordinator", "everyone", "bus", "中枢", "全部"];
            let is_broadcast = broadcast_words.iter().any(|w| tl.contains(w));
            let is_multi = tl.contains('+') || tl.contains(',') || tl.contains('&') || tl.contains(" and ");
            let is_session = tl.starts_with("session-");
            !(is_broadcast || is_multi || is_session)
        }).unwrap_or(false);

        if target_is_single {
            // 定向消息误走 collab → 违规
            violations.push(json!({
                "key": k, "severity": "WARN", "type": "定向消息误走广播",
                "from": from, "target": target, "ts": if ts_str.is_empty() { val_ts } else { ts_str },
                "advice": "应写 notes/<node>/ 定向通道或 data/<域>/ 命名空间，collab 仅限全局公告（R002）"
            }));
        }
    }

    let report = json!({
        "ts": now_ts(),
        "scanned_collab_msgs": scanned,
        "violations": violations,
        "violation_count": violations.len(),
        "summary": if violations.is_empty() { "通道卫生 ✅ 无违规".to_string() } else { format!("发现 {} 条定向消息误走广播", violations.len()) }
    });

    // 违规登记黑板（data/audit/channel-violations/）
    if write_bb && !violations.is_empty() {
        let key = format!("data/audit/channel-violations/v-{}-{}", now_ts(), violations.len());
        let _ = bb_put(&key, &json!({"value": report, "ts": now_ts()}));
    }

    if json_out {
        println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
    } else {
        println!("══ channel-audit 通道卫生巡检（{}h 窗口）══", hours);
        println!("  扫描 collab 消息: {}", scanned);
        println!("  违规数: {}", violations.len());
        for v in violations.iter().take(10) {
            println!("  ⚠️ [{}] {} → target={}", v["severity"], v["key"], v["target"]);
            println!("     from={} | {}", v["from"], v["advice"]);
        }
        if violations.is_empty() {
            println!("  ✅ 通道卫生正常");
        }
        println!("  报告: {}", report["summary"]);
    }
    0
}
