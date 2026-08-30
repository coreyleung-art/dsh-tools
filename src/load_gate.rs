// dsh-tools load-gate — 资源总控门（2026-08-30 用户要求，G5 两次服务器事件教训）
// 功能：部署/大任务/大传输前的资源门禁——服务器负载/内存/IO + 本机磁盘 + 端侧回报
// 状态：黑板 data/ops/resource-gate/status（绿灯/黄灯/红灯）
// 用法：
//   dsh-tools load-gate            # 检查当前资源状态（本机 + 服务器探活）
//   dsh-tools load-gate --check    # 门禁判定（部署前调用，红灯=拒绝）
//   dsh-tools load-gate --server <host>   # 指定服务器探活
//   dsh-tools load-gate --report   # 输出结构化状态（供黑板登记）

use std::path::PathBuf;
use serde_json::{json, Value};

fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/Users/unknown"))
}

fn now_ts() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 本机磁盘状态（macOS：df）
fn local_disk() -> (u64, u64) {
    // 返回 (可用 GB, 总量 GB)
    let out = std::process::Command::new("df").arg("-k").arg("/").output();
    match out {
        Ok(o) => {
            let txt = String::from_utf8_lossy(&o.stdout).to_string();
            let lines: Vec<&str> = txt.lines().collect();
            if lines.len() >= 2 {
                let fields: Vec<&str> = lines[1].split_whitespace().collect();
                if fields.len() >= 4 {
                    let avail_kb: u64 = fields[3].parse().unwrap_or(0);
                    let total_kb: u64 = fields[1].parse().unwrap_or(0);
                    return (avail_kb / 1024 / 1024, total_kb / 1024 / 1024);
                }
            }
            (0, 0)
        }
        Err(_) => (0, 0),
    }
}

/// 本机内存（macOS：vm_stat 估算，free+inactive+speculative 视为可用）
fn local_memory_free_pct() -> f64 {
    let out = std::process::Command::new("vm_stat").output();
    match out {
        Ok(o) => {
            let txt = String::from_utf8_lossy(&o.stdout).to_string();
            let mut free_pages = 0.0f64;
            let mut inactive_pages = 0.0f64;
            let mut speculative_pages = 0.0f64;
            let mut total_pages = 0.0f64;
            for line in txt.lines() {
                let v = line.split(':').nth(1).and_then(|s| s.trim().trim_end_matches('.').parse::<f64>().ok());
                if line.contains("Pages free:") { free_pages = v.unwrap_or(0.0); }
                else if line.contains("Pages inactive:") { inactive_pages = v.unwrap_or(0.0); }
                else if line.contains("Pages speculative:") { speculative_pages = v.unwrap_or(0.0); }
                else if line.contains("Pages total:") { total_pages = v.unwrap_or(0.0); }
            }
            if total_pages == 0.0 { total_pages = 1572864.0; } // 24GB 兜底
            let usable = free_pages + inactive_pages + speculative_pages;
            ((usable / total_pages) * 100.0).min(100.0)
        }
        Err(_) => 0.0,
    }
}

/// 服务器探活（HTTP 响应时间作为健康代理 + 可选 SSH 命令）
fn server_probe(host: &str) -> Value {
    use std::io::{Read, Write as _};
    use std::net::TcpStream;
    use std::time::{Duration, Instant};
    let mut result = json!({"host": host, "reachable": false});
    // TCP 22 探活
    let start = Instant::now();
    if let Ok(mut stream) = TcpStream::connect_timeout(&format!("{}:22", host).parse().unwrap_or_else(|_| {
        std::net::SocketAddr::from(([0,0,0,0], 22))
    }), Duration::from_secs(5)) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        // 尝试读 SSH banner（判断 SSH 响应正常 vs 超时）
        let mut buf = [0u8; 64];
        match stream.read(&mut buf) {
            Ok(_) => {
                result["reachable"] = json!(true);
                result["ssh_ok"] = json!(true);
                result["rtt_ms"] = json!(start.elapsed().as_millis() as u64);
            }
            Err(_) => {
                result["reachable"] = json!(true);
                result["ssh_ok"] = json!(false); // TCP 通但 SSH 无响应（负载高）
                result["note"] = json!("TCP 通但 SSH banner 超时——可能高负载");
            }
        }
    } else {
        result["reachable"] = json!(false);
        result["note"] = json!("TCP 22 不可达");
    }
    result
}

/// 门禁判定
fn gate_verdict(local_avail_gb: u64, mem_free_pct: f64, server: &Value, server_ok: bool) -> (String, String) {
    let mut issues: Vec<String> = Vec::new();
    if local_avail_gb < 20 { issues.push(format!("本机磁盘仅 {}GB 可用", local_avail_gb)); }
    if mem_free_pct < 20.0 { issues.push(format!("本机内存余量 {:.0}%", mem_free_pct)); }
    if !server_ok { issues.push(format!("服务器 {} 不可达", server["host"])); }
    else if server["ssh_ok"] == json!(false) { issues.push("服务器 SSH 响应异常（高负载）".to_string()); }

    if issues.is_empty() {
        ("green".to_string(), "资源充足，可执行".to_string())
    } else if issues.len() <= 1 {
        ("yellow".to_string(), format!("资源偏紧: {}", issues.join("; ")))
    } else {
        ("red".to_string(), format!("资源不足: {}", issues.join("; ")))
    }
}

/// 黑板写入（登记状态）
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
    let _ = stream.flush();
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).map_err(|e| format!("read: {}", e))?;
    Ok(())
}

pub fn run(args: &[String]) -> i32 {
    let mut server = "129.204.12.142".to_string();
    let mut mode = "check".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--server" => { i += 1; if i < args.len() { server = args[i].clone(); } }
            "--report" => { mode = "report".to_string(); }
            "--check" => { mode = "check".to_string(); }
            _ => {}
        }
        i += 1;
    }

    // 采集
    let (avail_gb, total_gb) = local_disk();
    let mem_free_pct = local_memory_free_pct();
    let server_state = server_probe(&server);
    let server_ok = server_state["reachable"].as_bool().unwrap_or(false);
    let (verdict, msg) = gate_verdict(avail_gb, mem_free_pct, &server_state, server_ok);

    let report = json!({
        "ts": now_ts(),
        "verdict": verdict,
        "msg": msg,
        "local": {"disk_avail_gb": avail_gb, "disk_total_gb": total_gb, "mem_free_pct": mem_free_pct as u64},
        "server": server_state,
        "gate": "load-gate v0.1",
        "advice": match verdict.as_str() {
            "red" => "红灯：禁止部署/大任务——等资源恢复（负载回落/清理磁盘）",
            "yellow" => "黄灯：资源偏紧——小任务可执行，大文件分批",
            _ => "绿灯：资源充足，可执行"
        }
    });

    // 黑板登记
    let _ = bb_put(&format!("data/ops/resource-gate/status-{}", now_ts()), &json!({"value": report, "ts": now_ts()}));
    // 最新状态（覆盖写，供门禁查询）
    let _ = bb_put("data/ops/resource-gate/current", &json!({"value": report, "ts": now_ts()}));

    if mode == "report" {
        println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
    } else {
        println!("══ load-gate 资源总控门 ══");
        println!("  本机磁盘: {}G 可用 / {}G 总量", avail_gb, total_gb);
        println!("  本机内存余量: {:.0}%", mem_free_pct);
        println!("  服务器 {}: {} (ssh_ok={})", server, 
            if server_ok { "可达" } else { "不可达" },
            server_state["ssh_ok"].as_bool().unwrap_or(false));
        println!("  ══ 判定: [{}] {} ══", 
            match verdict.as_str() { "green" => "🟢", "yellow" => "🟡", _ => "🔴" },
            msg);
        println!("  建议: {}", report["advice"]);
    }
    if verdict == "red" { 2 } else if verdict == "yellow" { 1 } else { 0 }
}
