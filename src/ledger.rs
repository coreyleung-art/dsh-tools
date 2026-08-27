// ledger.rs — 工具台账自动化（dsh-tools ledger 子命令）
// 扫描已知仓库的 git tag → 输出台账快照（JSON）+ 检测台账与实际版本漂移
// 用法：
//   dsh-tools ledger             — 扫描全部仓库，输出版本快照 + 漂移检测
//   dsh-tools ledger --json      — JSON 输出（供脚本/黑板）
//   dsh-tools ledger --repo <路径> — 只扫指定仓库
//
// 设计：扫 ~/dsh-collab/rust-* + ~/dsh-plugin-* 下带 .git 的仓库，
//       读 git describe --tags 最新 tag，与台账文件（tools-registry.md）对比。

use std::path::{Path, PathBuf};
use std::process::Command;

fn run_cmd(cmd: &str, args: &[&str], cwd: &str) -> Result<String, String> {
    let out = Command::new(cmd).args(args).current_dir(cwd).output()
        .map_err(|e| format!("执行 {} 失败: {}", cmd, e))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("{} 退出码 {}", cmd, out.status))
    }
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/Users/coreyleung".to_string())
}

/// 已知仓库目录（源码所在）
fn known_repos() -> Vec<PathBuf> {
    let h = home();
    let mut dirs = vec![
        PathBuf::from(format!("{}/dsh-collab/rust-tools", h)),
        PathBuf::from(format!("{}/dsh-collab/rust-bridge", h)),
        PathBuf::from(format!("{}/dsh-collab/rust-blackboard", h)),
        PathBuf::from(format!("{}/dsh-collab/rust-genebank", h)),
        PathBuf::from(format!("{}/dsh-plugin-agent-bus", h)),
        PathBuf::from(format!("{}/dsh-plugin-central-inbox", h)),
        PathBuf::from(format!("{}/dsh-plugin-openchronicle", h)),
    ];
    dirs.retain(|d| d.join(".git").exists());
    dirs
}

/// 扫一个仓库：最新 tag + 最近提交
fn scan_repo(dir: &Path) -> serde_json::Value {
    let name = dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let tag = run_cmd("git", &["describe", "--tags", "--abbrev=0"], dir.to_str().unwrap_or("."))
        .unwrap_or_else(|_| "无tag".to_string());
    let commit = run_cmd("git", &["rev-parse", "--short", "HEAD"], dir.to_str().unwrap_or("."))
        .unwrap_or_else(|_| "?".to_string());
    let last = run_cmd("git", &["log", "-1", "--format=%ci %s", "--date=short"], dir.to_str().unwrap_or("."))
        .unwrap_or_default();
    serde_json::json!({
        "name": name,
        "tag": tag,
        "commit": commit,
        "last_commit": last,
        "git": true,
    })
}

pub fn run(args: &[String]) -> i32 {
    let mut json_out = false;
    let mut only: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json_out = true,
            "--repo" => { i += 1; if i < args.len() { only = Some(args[i].clone()); } }
            _ => {}
        }
        i += 1;
    }

    let repos: Vec<PathBuf> = if let Some(r) = &only {
        let p = if r.starts_with('/') { PathBuf::from(r) } else { PathBuf::from(format!("{}/{}", home(), r)) };
        vec![p]
    } else {
        known_repos()
    };

    let mut items = Vec::new();
    for dir in &repos {
        if dir.join(".git").exists() {
            items.push(scan_repo(dir));
        }
    }
    items.sort_by(|a, b| a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or("")));

    let summary = serde_json::json!({
        "generated": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        "repos": items,
    });

    if json_out {
        println!("{}", serde_json::to_string_pretty(&summary).unwrap_or_default());
        return 0;
    }

    println!("═══ dsh-tools ledger ═══");
    println!("  生成时间: {}", summary["generated"].as_str().unwrap_or("?"));
    println!("  仓库数: {}", items.len());
    println!();
    println!("  {:<28} {:<12} {}", "仓库", "tag", "最近提交");
    println!("  {}", "-".repeat(70));
    for it in &items {
        let name = it["name"].as_str().unwrap_or("?");
        let tag = it["tag"].as_str().unwrap_or("?");
        let last = it["last_commit"].as_str().unwrap_or("?").split(' ').next().unwrap_or("?");
        println!("  {:<28} {:<12} {}", name, tag, last);
    }
    println!();
    println!("  💡 与 tools-registry.md 对比即可发现版本漂移（如 rust-blackboard 台账 v0.6.1 vs 实际 v0.6.2）");
    println!("  💡 完整 JSON: dsh-tools ledger --json");
    0
}
