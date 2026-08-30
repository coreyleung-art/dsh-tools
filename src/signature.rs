// dsh-tools node-bridge signature — P1-3d 消息签名（Ed25519 防伪造，openssl CLI 零依赖）
// 设计：
//   - identity.json 挂 Ed25519 私钥（P1-3b 扩展：原 device_id+token，现加 key 字段）
//   - node-bridge 发送消息 → 对规范化 body 签名 → 附 signature（base64）+ signer_pub（公钥）
//   - 黑板接收时校验（v0.7.x 计划）
// 实现：openssl CLI 子进程（macOS/Windows 均预装 openssl 或可配置路径）

use std::path::PathBuf;
use std::process::Command;

pub fn home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/Users/unknown"))
}

pub fn identity_path() -> PathBuf {
    // 与 node-bridge --identity 一致：默认 ~/.dsh/identity.json
    home().join(".dsh/identity.json")
}

fn openssl_bin() -> &'static str {
    // 优先 OpenSSL 3.x（支持 ED25519）：macOS brew /opt/homebrew；系统 LibreSSL 3.x 不支持 ED25519
    if std::path::Path::new("/opt/homebrew/opt/openssl@3/bin/openssl").exists() {
        return "/opt/homebrew/opt/openssl@3/bin/openssl";
    }
    if std::path::Path::new("/usr/local/opt/openssl@3/bin/openssl").exists() {
        return "/usr/local/opt/openssl@3/bin/openssl";
    }
    "openssl" // Windows/其他：PATH 需含 OpenSSL 3.x
}

/// 生成 Ed25519 密钥对（若 identity.json 无 key 字段），返回私钥/公钥 PEM
pub fn ensure_keys(identity_file: &std::path::Path) -> Result<(String, String), String> {
    // 已存在则读
    if let Ok(content) = std::fs::read_to_string(identity_file) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
            if let (Some(priv_k), Some(pub_k)) = (
                v.get("ed25519_private").and_then(|x| x.as_str()),
                v.get("ed25519_public").and_then(|x| x.as_str()),
            ) {
                return Ok((priv_k.to_string(), pub_k.to_string()));
            }
        }
    }
    // 生成新密钥对
    let priv_pem = gen_private_key()?;
    let pub_pem = derive_public_key(&priv_pem)?;
    // 写入 identity.json（保留原字段）
    let mut v = if let Ok(content) = std::fs::read_to_string(identity_file) {
        serde_json::from_str::<serde_json::Value>(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    v["ed25519_private"] = serde_json::Value::String(priv_pem.clone());
    v["ed25519_public"] = serde_json::Value::String(pub_pem.clone());
    v["signature_scheme"] = serde_json::Value::String("ed25519".into());
    if let Some(parent) = identity_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(identity_file, serde_json::to_string_pretty(&v).unwrap_or_default());
    Ok((priv_pem, pub_pem))
}

/// 生成 Ed25519 私钥 PEM
fn gen_private_key() -> Result<String, String> {
    let out = Command::new(openssl_bin())
        .args(["genpkey", "-algorithm", "ED25519", "-outform", "PEM"])
        .output()
        .map_err(|e| format!("openssl genpkey: {}", e))?;
    if !out.status.success() {
        return Err(format!("openssl genpkey failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// 从私钥推导公钥 PEM
fn derive_public_key(priv_pem: &str) -> Result<String, String> {
    // 写私钥到临时文件
    let tmp = std::env::temp_dir().join(format!("dsh-sign-{}.pem", std::process::id()));
    std::fs::write(&tmp, priv_pem).map_err(|e| format!("write tmp key: {}", e))?;
    let out = Command::new(openssl_bin())
        .args(["pkey", "-in", tmp.to_str().unwrap_or(""), "-pubout", "-outform", "PEM"])
        .output()
        .map_err(|e| format!("openssl pkey: {}", e))?;
    let _ = std::fs::remove_file(&tmp);
    if !out.status.success() {
        return Err(format!("openssl pkey failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// 对消息 body 签名，返回 base64 签名
pub fn sign_message(priv_pem: &str, body: &str) -> Result<String, String> {
    let tmp_key = std::env::temp_dir().join(format!("dsh-signkey-{}.pem", std::process::id()));
    let tmp_msg = std::env::temp_dir().join(format!("dsh-signmsg-{}.txt", std::process::id()));
    std::fs::write(&tmp_key, priv_pem).map_err(|e| format!("write key: {}", e))?;
    std::fs::write(&tmp_msg, body).map_err(|e| format!("write msg: {}", e))?;
    let out = Command::new(openssl_bin())
        .args(["pkeyutl", "-sign", "-inkey", tmp_key.to_str().unwrap_or(""),
               "-rawin", "-in", tmp_msg.to_str().unwrap_or("")])
        .output()
        .map_err(|e| format!("openssl sign: {}", e))?;
    let _ = std::fs::remove_file(&tmp_key);
    let _ = std::fs::remove_file(&tmp_msg);
    if !out.status.success() {
        return Err(format!("openssl sign failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(base64_encode(&out.stdout))
}

/// 验证签名（公钥 PEM + body + 签名 base64）
pub fn verify_message(pub_pem: &str, body: &str, sig_b64: &str) -> bool {
    let tmp_pub = std::env::temp_dir().join(format!("dsh-verifypub-{}.pem", std::process::id()));
    let tmp_msg = std::env::temp_dir().join(format!("dsh-verifymsg-{}.txt", std::process::id()));
    let tmp_sig = std::env::temp_dir().join(format!("dsh-verifysig-{}.bin", std::process::id()));
    let sig_bytes = match base64_decode(sig_b64) {
        Some(b) => b,
        None => { return false; }
    };
    let _ = std::fs::write(&tmp_pub, pub_pem);
    let _ = std::fs::write(&tmp_msg, body);
    let _ = std::fs::write(&tmp_sig, &sig_bytes);
    let out = Command::new(openssl_bin())
        .args(["pkeyutl", "-verify", "-pubin", "-inkey", tmp_pub.to_str().unwrap_or(""),
               "-rawin", "-in", tmp_msg.to_str().unwrap_or(""),
               "-sigfile", tmp_sig.to_str().unwrap_or("")])
        .output();
    let _ = std::fs::remove_file(&tmp_pub);
    let _ = std::fs::remove_file(&tmp_msg);
    let _ = std::fs::remove_file(&tmp_sig);
    match out {
        Ok(o) => {
            // openssl verify 成功输出 "Signature Verified Successfully" 且 exit 0
            o.status.success() && String::from_utf8_lossy(&o.stdout).contains("Verified")
        }
        Err(_) => false,
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 { out.push(TABLE[(n >> 6) as usize & 63] as char); } else { out.push('='); }
        if chunk.len() > 2 { out.push(TABLE[n as usize & 63] as char); } else { out.push('='); }
    }
    out
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let clean: String = s.chars().filter(|c| *c != '=' && !c.is_whitespace()).collect();
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0u8;
    for c in clean.chars() {
        let v = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            _ => return None,
        };
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// CLI：生成密钥 / 签名 / 验证
pub fn cli(args: &[String]) -> i32 {
    if args.is_empty() {
        println!("dsh-tools sign 子命令:");
        println!("  sign gen [--identity <path>]      # 生成 Ed25519 密钥对挂 identity.json");
        println!("  sign <body>                       # 用 identity.json 私钥签名（输出 base64）");
        println!("  sign verify <pub_pem> <body> <sig> # 验证签名");
        return 0;
    }
    match args[0].as_str() {
        "gen" => {
            let mut id_path = identity_path();
            let mut i = 1;
            while i < args.len() {
                if args[i] == "--identity" && i + 1 < args.len() { id_path = PathBuf::from(&args[i+1]); i += 2; continue; }
                i += 1;
            }
            match ensure_keys(&id_path) {
                Ok((_, pub_k)) => {
                    println!("✅ Ed25519 密钥已生成，挂载 {}", id_path.display());
                    println!("公钥:\n{}", pub_k);
                    0
                }
                Err(e) => { println!("❌ {}", e); 1 }
            }
        }
        "verify" if args.len() >= 4 => {
            let ok = verify_message(&args[1], &args[2], &args[3]);
            println!("{}", if ok { "✅ 签名验证通过" } else { "❌ 签名验证失败" });
            if ok { 0 } else { 1 }
        }
        body => {
            // 签名模式：用 identity.json 私钥签 body
            let id_path = identity_path();
            match ensure_keys(&id_path) {
                Ok((priv_k, _)) => match sign_message(&priv_k, body) {
                    Ok(sig) => { println!("{}", sig); 0 }
                    Err(e) => { println!("❌ {}", e); 1 }
                },
                Err(e) => { println!("❌ {}", e); 1 }
            }
        }
    }
}
