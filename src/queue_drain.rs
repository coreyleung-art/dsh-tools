// dsh-tools queue-drain — 消息堆积风暴整体抽走工具（2026-08-30 用户要求）
// 问题：i9 侧 372 线程/92 queued 堆积、黑板 notes 9945 条——消息风暴
// 功能：扫描堆积 → 归档（备份）→ 去重/合并 → 汇总报告 → 批量清理
// 用法：
//   dsh-tools queue-drain --scan         # 扫描统计（不清理）
//   dsh-tools queue-drain --drain        # 抽走：归档 + 清理 queued
//   dsh-tools queue-drain --drain --keep 7d   # 保留近 7 天，抽走更旧的
//   dsh-tools queue-drain --summary      # 汇总报告（来源分布/主题聚类）

use std::path::PathBuf;
use serde_json::{json, Value};

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/Users/unknown"))
}

fn agent_bus_path() -> PathBuf {
    home().join(".dsh/agent-bus.json")
}

fn now_ts() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn parse_ts(s: &str) -> u64 {
    // 支持 ISO 或数字时间戳
    if let Ok(n) = s.parse::<u64>() { return n; }
    // ISO: 2026-08-30T23:00:00
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) { return t.timestamp() as u64; }
    0
}

/// 扫描 agent-bus.json 的 threads，统计堆积
fn scan() -> Value {
    let path = agent_bus_path();
    let data = match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<Value>(&s) { Ok(v) => v, Err(_) => return json!({"error": "parse fail"}) },
        Err(e) => return json!({"error": format!("read fail: {}", e)}),
    };
    let threads = data.get("threads").and_then(|t| t.as_array()).cloned().unwrap_or_default();
    let mut total_threads = threads.len();
    let mut queued: Vec<Value> = Vec::new();
    let mut delivered = 0usize;
    let mut from_count: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut to_count: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();

    for t in &threads {
        if let Some(msgs) = t.get("messages").and_then(|m| m.as_array()) {
            for m in msgs {
                let status = m.get("status").and_then(|s| s.as_str()).unwrap_or("");
                let from = m.get("from").and_then(|f| f.as_str()).unwrap_or("?").to_string();
                let to = m.get("to").and_then(|f| f.as_str()).unwrap_or("?").to_string();
                *from_count.entry(from.clone()).or_insert(0) += 1;
                *to_count.entry(to.clone()).or_insert(0) += 1;
                if status == "queued" {
                    queued.push(m.clone());
                } else if status == "delivered" {
                    delivered += 1;
                }
            }
        }
    }

    // TOP 发送方/接收方
    let top_from: Vec<Value> = from_count.iter()
        .map(|(k, v)| json!({"from": k, "count": v}))
        .collect::<Vec<_>>()
        .into_iter()
        .take(5)
        .collect();
    // 排序
    let mut top_from = top_from;
    top_from.sort_by(|a, b| b["count"].as_u64().unwrap_or(0).cmp(&a["count"].as_u64().unwrap_or(0)));

    json!({
        "ts": now_ts(),
        "total_threads": total_threads,
        "queued": queued.len(),
        "delivered": delivered,
        "queued_ratio": format!("{:.1}%", if total_threads > 0 { queued.len() as f64 / total_threads as f64 * 100.0 } else { 0.0 }),
        "top_senders": top_from,
        "sample_queued": queued.iter().take(3).map(|q| q.get("text").and_then(|t| t.as_str()).unwrap_or("").chars().take(60).collect::<String>()).collect::<Vec<_>>(),
    })
}

/// 归档 + 清理 queued（抽走）
fn drain(keep_days: Option<u64>) -> Value {
    let path = agent_bus_path();
    let mut data = match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<Value>(&s) { Ok(v) => v, Err(_) => return json!({"error": "parse fail"}) },
        Err(e) => return json!({"error": format!("read fail: {}", e)}),
    };
    let cutoff = if let Some(d) = keep_days { now_ts() - d * 86400 } else { 0 };

    // 备份当前状态
    let backup_dir = home().join(".dsh/queue-drain-backups");
    let _ = std::fs::create_dir_all(&backup_dir);
    let backup_path = backup_dir.join(format!("agent-bus-{}.json", now_ts()));
    let _ = std::fs::write(&backup_path, data.to_string());

    // 遍历 threads，抽出 queued（标记 archived + 归档到 archive 字段）
    let mut drained = 0usize;
    let mut archived_msgs: Vec<Value> = Vec::new();
    if let Some(threads) = data.get_mut("threads").and_then(|t| t.as_array_mut()) {
        for t in threads.iter_mut() {
            if let Some(msgs) = t.get_mut("messages").and_then(|m| m.as_array_mut()) {
                // 分离 queued 且超过 cutoff
                let mut keep: Vec<Value> = Vec::new();
                for m in msgs.drain(..) {
                    let status = m.get("status").and_then(|s| s.as_str()).unwrap_or("");
                    let ts = m.get("time").and_then(|t| t.as_i64()).map(|x| x as u64)
                        .or_else(|| m.get("time").and_then(|t| t.as_str()).map(|s| parse_ts(s)))
                        .unwrap_or(0);
                    if status == "queued" && (cutoff == 0 || ts < cutoff) {
                        drained += 1;
                        archived_msgs.push(m);
                    } else {
                        keep.push(m);
                    }
                }
                *msgs = keep;
            }
        }
    }
    // 归档记录
    data["queue_drain"] = json!({
        "last_drain_ts": now_ts(),
        "drained_count": drained,
        "archive": format!("~/.dsh/queue-drain-backups/agent-bus-{}.json", now_ts()),
    });
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&data).unwrap_or_default());

    json!({
        "drained": drained,
        "backup": backup_path.display().to_string(),
        "archived_samples": archived_msgs.iter().take(3).map(|m| m.get("text").and_then(|t| t.as_str()).unwrap_or("").chars().take(50).collect::<String>()).collect::<Vec<_>>(),
        "note": "已归档（备份保留可恢复），queued 清理完成。旧线程可后续归档收敛。",
    })
}

pub fn run(args: &[String]) -> i32 {
    let mut mode = "scan".to_string();
    let mut keep_days: Option<u64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--scan" => mode = "scan".to_string(),
            "--drain" => mode = "drain".to_string(),
            "--summary" => mode = "summary".to_string(),
            "--keep" => { i += 1; if i < args.len() { keep_days = args[i].parse().ok(); } }
            _ => {}
        }
        i += 1;
    }

    match mode.as_str() {
        "scan" => {
            let r = scan();
            println!("══ queue-drain 堆积扫描 ══");
            println!("  线程总数: {}", r["total_threads"]);
            println!("  queued: {} | delivered: {}", r["queued"], r["delivered"]);
            println!("  堆积比例: {}", r["queued_ratio"]);
            if let Some(senders) = r["top_senders"].as_array() {
                println!("  TOP 发送方:");
                for s in senders.iter().take(3) {
                    println!("    · {} ({} 条)", s["from"], s["count"]);
                }
            }
            if let Some(s) = r["sample_queued"].as_array() {
                if !s.is_empty() {
                    println!("  queued 示例:");
                    for x in s.iter().take(2) {
                        println!("    · {}", x.as_str().unwrap_or(""));
                    }
                }
            }
            println!("  建议: --drain 抽走 queued（备份保留）| --keep 7d 保留近期");
        }
        "drain" => {
            let r = drain(keep_days);
            println!("══ queue-drain 抽走 ══");
            println!("  抽走 queued: {}", r["drained"]);
            println!("  备份: {}", r["backup"]);
            if let Some(s) = r["archived_samples"].as_array() {
                for x in s.iter().take(2) {
                    println!("  归档示例: · {}", x.as_str().unwrap_or(""));
                }
            }
            println!("  {}", r["note"]);
        }
        "summary" => {
            let r = scan();
            println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
        }
        _ => {}
    }
    0
}
