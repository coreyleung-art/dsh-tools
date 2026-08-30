// dsh-tools queue-condense — 堆积队列语义浓缩（Rust 版，2026-08-30 语言评估后 Rust 化）
// 功能：读 agent-bus.json queued → 去重 → bge-m3 聚类 → qwen 归纳 → 汇总写黑板 + 通知会话
// 依赖：serde_json + Ollama HTTP（std 网络）+ 自实现余弦聚类（无 chromadb 硬依赖）
// R006 工具化：CLI + 版本 + 日志 + 落链（黑板汇总）
// 用法：
//   dsh-tools queue-condense --scan          # 扫描统计
//   dsh-tools queue-condense --condense --to <会话>   # 完整管道（去重→聚类→归纳→写黑板）
//   dsh-tools queue-condense --dry-run       # 干跑（不去重不归纳不写）

use std::path::PathBuf;
use serde_json::{json, Value};

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/Users/unknown"))
}

fn now_ts() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Ollama HTTP POST（embed / generate）
fn ollama(path: &str, body: &Value) -> Result<Value, String> {
    use std::io::{Read, Write as _};
    use std::net::TcpStream;
    use std::time::Duration;
    let mut stream = TcpStream::connect("127.0.0.1:11434").map_err(|e| format!("connect: {}", e))?;
    stream.set_read_timeout(Some(Duration::from_secs(120))).ok();
    let body_str = body.to_string();
    let req = format!(
        "POST /{} HTTP/1.1\r\nHost: 127.0.0.1:11434\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path, body_str.len(), body_str
    );
    stream.write_all(req.as_bytes()).map_err(|e| format!("write: {}", e))?;
    let _ = stream.flush();
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).map_err(|e| format!("read: {}", e))?;
    let text = String::from_utf8_lossy(&resp).to_string();
    // 用字节级精确解 chunked（避免 \r\n 边界误判）
    let raw = text.split("\r\n\r\n").nth(1).unwrap_or("").as_bytes().to_vec();
    let mut body: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while i < raw.len() {
        // 读块大小（十六进制到 \r\n）
        let mut size_end = i;
        while size_end < raw.len() && raw[size_end] != b'\r' { size_end += 1; }
        if size_end >= raw.len() { break; }
        let size_str = String::from_utf8_lossy(&raw[i..size_end]).to_string();
        let size = match usize::from_str_radix(size_str.trim(), 16) { Ok(n) => n, Err(_) => break };
        if size == 0 { break; } // 结束
        let data_start = size_end + 2; // 跳过 \r\n
        if data_start + size > raw.len() { break; }
        body.extend_from_slice(&raw[data_start..data_start + size]);
        i = data_start + size + 2; // 跳过数据 + 尾部 \r\n
    }
    let body_str = String::from_utf8_lossy(&body).to_string();
    serde_json::from_str(&body_str).map_err(|e| format!("parse: {}", e))
}

fn embed(text: &str) -> Result<Vec<f64>, String> {
    let r = ollama("api/embed", &json!({"model": "bge-m3", "input": [text]}))?;
    r.get("embeddings").and_then(|e| e.get(0)).and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_f64()).collect())
        .ok_or_else(|| "embed 无结果".to_string())
}

fn llm(prompt: &str) -> Result<String, String> {
    let r = ollama("api/generate", &json!({
        "model": "qwen2.5:3b", "prompt": prompt, "stream": false,
        "options": {"temperature": 0.3, "num_predict": 400}
    }))?;
    r.get("response").and_then(|x| x.as_str()).map(|s| s.to_string())
        .ok_or_else(|| "generate 无结果".to_string())
}

fn cosine(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() || a.is_empty() { return 0.0; }
    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na * nb) }
}

/// 读 agent-bus.json 的 queued 消息
fn load_queued() -> Vec<Value> {
    let path = home().join(".dsh/agent-bus.json");
    let data = match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<Value>(&s) { Ok(v) => v, Err(_) => return vec![] },
        Err(_) => return vec![],
    };
    let mut msgs = Vec::new();
    if let Some(threads) = data.get("threads").and_then(|t| t.as_array()) {
        for t in threads {
            if let Some(ms) = t.get("messages").and_then(|m| m.as_array()) {
                for m in ms {
                    if m.get("status").and_then(|s| s.as_str()).unwrap_or("") == "queued" {
                        msgs.push(json!({
                            "from": m.get("from").and_then(|f| f.as_str()).unwrap_or("?"),
                            "to": m.get("to").and_then(|f| f.as_str()).unwrap_or("?"),
                            "text": m.get("text").and_then(|t| t.as_str()).unwrap_or(""),
                        }));
                    }
                }
            }
        }
    }
    msgs
}

/// 黑板 PUT
fn bb_put(path: &str, v: &Value) -> Result<(), String> {
    use std::io::{Read, Write as _};
    use std::net::TcpStream;
    use std::time::Duration;
    let mut stream = TcpStream::connect("127.0.0.1:8792").map_err(|e| format!("connect: {}", e))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let body = v.to_string();
    let req = format!(
        "PUT /{} HTTP/1.1\r\nHost: 127.0.0.1:8792\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path, body.len(), body
    );
    stream.write_all(req.as_bytes()).map_err(|e| format!("write: {}", e))?;
    let _ = stream.flush();
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).map_err(|e| format!("read: {}", e))?;
    Ok(())
}

pub fn run(args: &[String]) -> i32 {
    let mut mode = "scan".to_string();
    let mut to_session: Option<String> = None;
    let mut dry = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--scan" => mode = "scan".to_string(),
            "--condense" => mode = "condense".to_string(),
            "--dry-run" => { mode = "condense".to_string(); dry = true; }
            "--to" => { i += 1; if i < args.len() { to_session = Some(args[i].clone()); } }
            _ => {}
        }
        i += 1;
    }

    let msgs = load_queued();
    if mode == "scan" {
        println!("══ queue-condense 扫描 ══");
        println!("  queued: {}", msgs.len());
        if msgs.is_empty() { println!("  无堆积"); return 0; }
        // 粗略重复
        let mut texts = std::collections::HashSet::new();
        let mut dup = 0;
        for m in &msgs {
            let t = m["text"].as_str().unwrap_or("").trim();
            if !t.is_empty() && !texts.insert(t.to_string()) { dup += 1; }
        }
        println!("  精确重复: {}", dup);
        println!("  建议: --condense 去重→聚类→归纳→写黑板 | --dry-run 干跑");
        return 0;
    }

    if msgs.is_empty() { println!("无 queued 消息"); return 0; }
    println!("══ queue-condense 管道 ══");
    println!("  原始 queued: {}", msgs.len());

    // ① 去重
    let mut seen = std::collections::HashSet::new();
    let mut unique: Vec<Value> = Vec::new();
    for m in &msgs {
        let t = m["text"].as_str().unwrap_or("").trim().to_string();
        if t.is_empty() || seen.contains(&t) { continue; }
        seen.insert(t);
        unique.push(m.clone());
    }
    println!("  去重后: {}（-{} 重复）", unique.len(), msgs.len() - unique.len());

    if dry {
        println!("[dry-run] 跳过 embed/聚类/归纳/写黑板");
        return 0;
    }

    // ② bge-m3 聚类（贪心：逐条对比已有簇质心，>0.55 并入）
    let mut clusters: Vec<(Vec<f64>, Vec<Value>)> = Vec::new();
    for m in &unique {
        let text = m["text"].as_str().unwrap_or("").chars().take(200).collect::<String>();
        match embed(&text) {
        Ok(v) => {
            let mut placed = false;
            for c in clusters.iter_mut() {
                if cosine(&c.0, &v) > 0.55 {
                    c.1.push(m.clone());
                    placed = true;
                    break;
                }
            }
            if !placed { clusters.push((v, vec![m.clone()])); }
        }
        Err(e) => eprintln!("[embed] {} 失败: {}", &text[..text.len().min(20)], e),
        }
    }
    println!("  语义聚类: {} 簇", clusters.len());

    // ③ 大簇 qwen 归纳（top 5）
    clusters.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    let mut summary = json!({"ts": now_ts(), "source": msgs.len(), "unique": unique.len(), "clusters": []});
    let mut cluster_arr = Vec::new();
    for (idx, c) in clusters.iter().enumerate().take(5) {
        let mut summ = String::new();
        if c.1.len() >= 3 {
            let sample: Vec<String> = c.1.iter().take(8)
                .map(|m| format!("- [{}→{}] {}", m["from"], m["to"], m["text"].as_str().unwrap_or("").chars().take(120).collect::<String>()))
                .collect();
            let prompt = format!(
                "以下是一簇语义相近的消息（{} 条），归纳：①主题（一句话）②核心要点（2-3条）③是否仍需处理（历史/重复/已过时）\n消息：\n{}",
                c.1.len(), sample.join("\n")
            );
            match llm(&prompt) {
                Ok(s) => summ = s,
                Err(e) => summ = format!("(归纳失败: {})", e),
            }
            println!("  [归纳簇{} ({}条)] ...", idx + 1, c.1.len());
        } else {
            summ = format!("(小簇 {} 条) {}", c.1.len(), c.1[0]["text"].as_str().unwrap_or("").chars().take(60).collect::<String>());
        }
        cluster_arr.push(json!({
            "id": idx + 1, "count": c.1.len(),
            "samples": c.1.iter().take(3).map(|m| m["text"].as_str().unwrap_or("").chars().take(60).collect::<String>()).collect::<Vec<_>>(),
            "summary": summ
        }));
    }
    summary["clusters"] = json!(cluster_arr);

    // ④ 写黑板 + 通知会话
    let key = format!("data/ops/queue-condense/condense-{}", now_ts());
    let _ = bb_put(&key, &json!({"value": {"type": "queue-condense", "summary": summary, "target": to_session, "ts": now_ts()}, "ts": now_ts()}));
    println!("✅ 汇总已写黑板 {}", key);
    if let Some(t) = &to_session {
        let short: String = t.chars().take(8).collect();
        let note_key = format!("notes/{}/queue-condense-{}", short, now_ts());
        let _ = bb_put(&note_key, &json!({"value": {"from": "queue-condense", "to": t,
            "body": format!("看黑板 {}（{}条→{}簇汇总，一次性处理）", key, msgs.len(), clusters.len()),
            "ts": now_ts()}, "ts": now_ts()}));
        println!("✅ 已通知 {} 处理", t);
    }
    0
}
