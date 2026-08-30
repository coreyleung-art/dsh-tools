// dsh-tools noise-report / noise-scan — 无关消息反馈链（R018 候选，2026-08-30 用户要求）
// 机制：任何智能体收到与自己无关的 send 消息 → noise-report 写黑板 data/audit/noise-reports/
//       HR（司库）noise-scan 定时聚合 → 识别重复污染源 → 介入治理
// 用法：
//   dsh-tools noise-report --from <发送方> --to <自己> --why <为何无关> [--key <消息key>]
//   dsh-tools noise-scan [--hours N] [--json]     # HR 聚合：TOP 污染源 + 重复模式

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

/// ① 端侧反馈：记录一条无关消息
fn cmd_report(args: &[String]) -> i32 {
    let mut from = String::new();
    let mut to = String::new();
    let mut why = String::new();
    let mut key = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => { i += 1; if i < args.len() { from = args[i].clone(); } }
            "--to" => { i += 1; if i < args.len() { to = args[i].clone(); } }
            "--why" => { i += 1; if i < args.len() { why = args[i].clone(); } }
            "--key" => { i += 1; if i < args.len() { key = args[i].clone(); } }
            _ => {}
        }
        i += 1;
    }
    if from.is_empty() || why.is_empty() {
        println!("用法: dsh-tools noise-report --from <发送方> --to <自己> --why <为何无关> [--key <消息key>]");
        return 1;
    }
    let ts = now_ts();
    let key_path = format!("data/audit/noise-reports/n-{}-{}", ts, from.chars().take(8).collect::<String>());
    let report = json!({
        "value": {
            "from_sender": from, "receiver": to, "why_irrelevant": why,
            "msg_key": key, "ts": ts,
            "note": "R018 反馈链：收到与自己无关的 send 消息，反馈 HR（司库）捕获网治理"
        },
        "ts": ts
    });
    match bb_put(&key_path, &report) {
        Ok(_) => {
            println!("✅ 已反馈 HR: data/audit/noise-reports/ (sender={}, why={})", from, why.chars().take(40).collect::<String>());
            0
        }
        Err(e) => { println!("❌ 反馈失败: {}", e); 1 }
    }
}

/// ② HR 聚合扫描：识别重复污染源
fn cmd_scan(args: &[String]) -> i32 {
    let mut hours = 24u64;
    let mut json_out = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--hours" => { i += 1; if i < args.len() { hours = args[i].parse().unwrap_or(24); } }
            "--json" => { json_out = true; }
            _ => {}
        }
        i += 1;
    }
    let data = match bb_get("data?node=audit") {
        Ok(d) => d,
        Err(e) => { println!("{}", json!({"error": e})); return 1; }
    };
    let list = data.get("list").and_then(|l| l.as_object()).cloned().unwrap_or_default();
    let cutoff = now_ts() - hours * 3600;
    let mut reports: Vec<Value> = Vec::new();
    let mut sender_counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut why_by_sender: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();

    for (k, v) in list.iter() {
        if !k.starts_with("data/audit/noise-reports/") { continue; }
        let vv = v.get("value").cloned().unwrap_or(json!({}));
        let val_obj = vv.get("value").cloned().unwrap_or(json!({}));
        let sender = val_obj.get("from_sender").and_then(|x| x.as_str()).unwrap_or("?").to_string();
        let why = val_obj.get("why_irrelevant").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let receiver = val_obj.get("receiver").and_then(|x| x.as_str()).unwrap_or("?").to_string();
        *sender_counts.entry(sender.clone()).or_insert(0) += 1;
        why_by_sender.entry(sender.clone()).or_default().push(why.clone());
        reports.push(json!({"key": k, "sender": sender, "receiver": receiver, "why": why.chars().take(60).collect::<String>()}));
    }

    // TOP 污染源（反馈次数排序）
    let mut top: Vec<Value> = sender_counts.iter()
        .map(|(s, c)| json!({"sender": s, "reports": c, "reasons": why_by_sender.get(s).unwrap_or(&vec![]).iter().take(2).cloned().collect::<Vec<_>>()}))
        .collect();
    top.sort_by(|a, b| b["reports"].as_u64().unwrap_or(0).cmp(&a["reports"].as_u64().unwrap_or(0)));

    let report = json!({
        "ts": now_ts(), "window_hours": hours,
        "total_reports": reports.len(),
        "top_sources": top.iter().take(5).collect::<Vec<_>>(),
        "pollution_suspected": reports.len() >= 3, // ≥3 次反馈 = 系统性污染，需 HR 介入
        "summary": if reports.is_empty() { "无反馈（通道卫生正常）".to_string() } else {
            format!("收到 {} 条无关消息反馈，疑似污染源 TOP: {}", reports.len(),
                top.iter().take(3).map(|t| format!("{}×{}", t["sender"], t["reports"])).collect::<Vec<_>>().join(", "))
        }
    });

    if json_out {
        println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
    } else {
        println!("══ noise-scan HR 捕获网（{}h）══", hours);
        println!("  无关消息反馈: {} 条", reports.len());
        if reports.is_empty() {
            println!("  ✅ 无反馈");
        } else {
            for t in top.iter().take(5) {
                let flag = if t["reports"].as_u64().unwrap_or(0) >= 3 { "🔴" } else { "🟡" };
                println!("  {} {} ({} 次反馈)", flag, t["sender"], t["reports"]);
                for r in t["reasons"].as_array().unwrap_or(&vec![]).iter().take(2) {
                    println!("      - {}", r.as_str().unwrap_or(""));
                }
            }
            if report["pollution_suspected"].as_bool().unwrap_or(false) {
                println!("  🔴 疑似系统性污染（≥3 次反馈），建议 HR 介入治理");
            }
        }
        println!("  {}", report["summary"]);
    }
    0
}

pub fn run(args: &[String]) -> i32 {
    if args.is_empty() {
        println!("dsh-tools noise 子命令:");
        println!("  noise-report --from <发送方> --to <自己> --why <为何无关> [--key <key>]  # 端侧反馈（收到无关消息时）");
        println!("  noise-scan [--hours N] [--json]   # HR 捕获网聚合（TOP 污染源 + 系统性污染识别）");
        return 0;
    }
    match args[0].as_str() {
        "report" | "noise-report" => cmd_report(&args[1..]),
        "scan" | "noise-scan" => cmd_scan(&args[1..]),
        other => { println!("未知 noise 子命令: {}", other); 1 }
    }
}
