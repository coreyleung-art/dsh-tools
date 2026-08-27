// tailnet_proxy.rs — dsh 远程访问入口（Rust 版，替代 dsh-tailnet-proxy.mjs）
// 安全暴露面组件（多设备可达）→ 按选型矩阵用 Rust（编译期内存安全更硬）
// 功能：绑定 tailnet IP → 自动发现 GUI 端口（lsof）→ HTTP/WS 代理 → Host/Origin 改写 loopback
// 安全语义：改写使 dsh /api 围栏按 loopback 放行，暴露边界必须收在 tailnet（Tailscale 身份+ACL 即信任锚点）

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::time::Duration;

fn discover_tailnet_ip() -> Option<String> {
    let out = Command::new("ifconfig").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    for line in text.lines() {
        if let Some(i) = line.find("inet ") {
            let rest = &line[i + 5..];
            let ip: String = rest.split_whitespace().next().unwrap_or("").to_string();
            if ip.starts_with("100.") {
                return Some(ip);
            }
        }
    }
    None
}

/// lsof 发现 CLD GUI 的 loopback 监听端口（≥4000，排除低端口误判）
fn discover_gui_port() -> u16 {
    let out = match Command::new("lsof").args(["-nP", "-iTCP", "-sTCP:LISTEN"]).output() {
        Ok(o) => o,
        Err(_) => return 0,
    };
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut ports: Vec<u16> = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 9 { continue; }
        if parts[0] != "CLD" { continue; }
        // 形如 127.0.0.1:49959 (LISTEN)
        if let Some(addr) = parts[8].split(':').nth(1) {
            if let Ok(p) = addr.parse::<u16>() {
                if p >= 4000 { ports.push(p); }
            }
        }
    }
    // 取最后一个有效端口（GUI 重启换端口后跟随）
    ports.last().copied().unwrap_or(0)
}

fn loopback_headers(headers: &str, port: u16) -> String {
    // 改写 Host 和 Origin 为 127.0.0.1:port（保持其他头原样）
    let mut out = String::new();
    for line in headers.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("host:") {
            out.push_str(&format!("Host: 127.0.0.1:{}\r\n", port));
        } else if lower.starts_with("origin:") {
            out.push_str(&format!("Origin: http://127.0.0.1:{}\r\n", port));
        } else {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out
}

fn proxy_http(mut client: TcpStream, target_port: u16, method: &str, path: &str, headers: &str, body: &[u8]) {
    // 连接 GUI
    let mut upstream = match TcpStream::connect(("127.0.0.1", target_port)) {
        Ok(s) => s,
        Err(e) => {
            let resp = format!("HTTP/1.1 502 Bad Gateway\r\nContent-Length: 11\r\nConnection: close\r\n\r\nbad gateway");
            let _ = client.write_all(resp.as_bytes());
            eprintln!("[proxy] connect gui:{} failed: {}", target_port, e);
            return;
        }
    };
    upstream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    upstream.set_write_timeout(Some(Duration::from_secs(30))).ok();

    // 构造转发请求
    let mut req = format!("{} {} HTTP/1.1\r\n", method, path);
    req.push_str(&loopback_headers(headers, target_port));
    // 去 hop-by-hop
    let filtered: String = req.lines().filter(|l| {
        let l = l.to_lowercase();
        !l.starts_with("connection:") && !l.starts_with("upgrade:")
    }).collect::<Vec<_>>().join("\r\n");
    let mut final_req = filtered;
    final_req.push_str("\r\n");
    if !body.is_empty() {
        final_req.push_str("\r\n");
    }
    let _ = upstream.write_all(final_req.as_bytes());
    if !body.is_empty() {
        let _ = upstream.write_all(body);
    }

    // 转发响应回客户端（简单模型：读全部再写）
    let mut resp = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match upstream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => resp.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    let _ = client.write_all(&resp);
}

fn proxy_upgrade(mut client: TcpStream, target_port: u16, headers: &str) {
    // WebSocket Upgrade：裸 TCP 双向转发（简化：只处理握手后的双向流）
    let mut upstream = match TcpStream::connect(("127.0.0.1", target_port)) {
        Ok(s) => s,
        Err(_) => { let _ = client.shutdown(std::net::Shutdown::Both); return; }
    };
    upstream.set_read_timeout(Some(Duration::from_secs(600))).ok();
    upstream.set_write_timeout(Some(Duration::from_secs(600))).ok();
    client.set_read_timeout(Some(Duration::from_secs(600))).ok();
    client.set_write_timeout(Some(Duration::from_secs(600))).ok();

    let req = format!("{} HTTP/1.1\r\n{}", "GET", loopback_headers(headers, target_port));
    let _ = upstream.write_all(req.as_bytes());

    // 双向管道（两个线程）
    let mut c2 = client.try_clone().unwrap();
    let mut u2 = upstream.try_clone().unwrap();
    let t1 = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match c2.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => { if u2.write_all(&buf[..n]).is_err() { break; } }
            }
        }
    });
    let mut buf = [0u8; 8192];
    loop {
        match upstream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => { if client.write_all(&buf[..n]).is_err() { break; } }
        }
    }
    let _ = t1.join();
}

pub fn run(bind_host: &str, bind_port: u16) {
    let listener = match TcpListener::bind((bind_host, bind_port)) {
        Ok(l) => l,
        Err(e) => { eprintln!("[proxy] bind {}:{} failed: {}", bind_host, bind_port, e); return; }
    };
    println!("[proxy] dsh 远程入口 {}:{} → 127.0.0.1:<GUI 端口>", bind_host, bind_port);

    let mut gui_port = 0u16;
    let mut last_scan = 0u64;

    for stream in listener.incoming() {
        let Ok(mut client) = stream else { continue };
        client.set_read_timeout(Some(Duration::from_secs(35))).ok();
        client.set_write_timeout(Some(Duration::from_secs(35))).ok();

        // 定时重扫 GUI 端口（60s）
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        if now - last_scan >= 60 || gui_port == 0 {
            let p = discover_gui_port();
            if p > 0 {
                gui_port = p;
                println!("[proxy] GUI 端口 = {}", gui_port);
            }
            last_scan = now;
        }
        if gui_port == 0 {
            let resp = "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 15\r\nConnection: close\r\n\r\ngui port unknown";
            let _ = client.write_all(resp.as_bytes());
            continue;
        }

        let gui = gui_port;
        std::thread::spawn(move || {
            // 读请求头
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                match client.read(&mut tmp) {
                    Ok(0) => return,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") { break; }
                        if buf.len() > 1_200_000 { return; }
                    }
                    Err(_) => return,
                }
            }
            let req_text = String::from_utf8_lossy(&buf).to_string();
            let first = req_text.lines().next().unwrap_or("").to_string();
            let parts: Vec<&str> = first.split_whitespace().collect();
            if parts.len() < 2 { return; }
            let method = parts[0].to_string();
            let target = parts[1].to_string();
            let (path, _query) = match target.find('?') {
                Some(i) => (target[..i].to_string(), target[i+1..].to_string()),
                None => (target, String::new()),
            };
            let headers = req_text.split("\r\n\r\n").nth(0).unwrap_or("").to_string();
            let body = req_text.split("\r\n\r\n").nth(1).unwrap_or("").as_bytes().to_vec();

            // 判断 Upgrade（WS）
            let is_upgrade = headers.to_lowercase().contains("upgrade: websocket");
            let body_copy = body.clone();
            let headers_copy = headers.clone();
            if is_upgrade {
                proxy_upgrade(client, gui, &headers_copy);
            } else {
                proxy_http(client, gui, &method, &path, &headers_copy, &body_copy);
            }
        });
    }
}
