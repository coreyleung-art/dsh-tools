// version.rs — 自动化版本管理（dsh-tools version 子命令）
// 用途：把「记录变更 → bump 版本 → CHANGELOG → git commit/tag → 推送」固化为一条命令，
//       迭代完成时自动调用，替代手动逐项操作。
//
// 用法：
//   dsh-tools version [--type fix|feat|breaking] [--desc '变更描述'] [--repo <路径>] [--dry-run]
//
// 流程：
//   1. 检测项目类型（package.json → node / Cargo.toml → rust）
//   2. 读当前版本，按类型计算新版本（fix→patch / feat→minor / breaking→major）
//   3. bump 版本文件（node: package.json / rust: Cargo.toml）
//   4. 追加 CHANGELOG 段（含描述 + 日期）
//   5. git add + commit（消息: v<新版本>: <desc>）
//   6. git tag v<新版本>
//   7. 推送（git push 分支 + tag；失败提示 REST 兜底）
//
// 设计：调外部 git（std::process::Command），无额外依赖；--dry-run 只预览不执行。

use std::fs;
use std::path::Path;
use std::process::Command;

const SEMVER_RE: &str = r"^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+([0-9A-Za-z.-]+))?$";

fn usage() {
    println!("用法: dsh-tools version [--type fix|feat|breaking] [--desc '变更描述'] [--repo <路径>] [--dry-run]");
    println!("  --type   变更类型: fix(补丁) / feat(新功能) / breaking(破坏性)   [默认 fix]");
    println!("  --desc   变更描述（写入 commit + CHANGELOG）                     [必填]");
    println!("  --repo   目标仓库路径（默认当前目录）");
    println!("  --dry-run 只预览不执行");
}

fn run_git(cmd: &str, args: &[&str], cwd: &str) -> Result<String, String> {
    let out = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("执行 {} 失败: {}", cmd, e))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("{} 退出码 {}: {}", cmd, out.status, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// 解析 semver，返回 (major, minor, patch)
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let caps = regex_capture(v)?;
    Some((caps.0, caps.1, caps.2))
}

/// 极简 semver 匹配（避免引入 regex 依赖，手写解析）
fn regex_capture(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim();
    // 去掉 pre-release / build 后缀
    let base = v.split(['-', '+']).next().unwrap_or(v);
    let mut parts = base.split('.');
    let m = parts.next()?.parse().ok()?;
    let n = parts.next()?.parse().ok()?;
    let p = parts.next()?.parse().ok()?;
    Some((m, n, p))
}

fn bump(current: &str, typ: &str) -> Result<(String, String), String> {
    let (m, n, p) = parse_version(current)
        .ok_or_else(|| format!("无法解析版本号: {}", current))?;
    let (nm, nn, np) = match typ {
        "breaking" => (m + 1, 0, 0),
        "feat" => (m, n + 1, 0),
        _ => (m, n, p + 1),
    };
    Ok((format!("{}.{}.{}", nm, nn, np), format!("{}.{}.{}", m, n, p)))
}

/// 检测项目类型并 bump 版本文件
fn bump_project(repo: &str, new_ver: &str) -> Result<(), String> {
    let pkg = Path::new(repo).join("package.json");
    let cargo = Path::new(repo).join("Cargo.toml");

    if pkg.exists() {
        // node 项目
        let raw = fs::read_to_string(&pkg).map_err(|e| format!("读 package.json: {}", e))?;
        let mut json: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| format!("解析 package.json: {}", e))?;
        json["version"] = serde_json::Value::String(new_ver.to_string());
        let pretty = serde_json::to_string_pretty(&json)
            .map_err(|e| format!("序列化 package.json: {}", e))?;
        fs::write(&pkg, pretty + "\n").map_err(|e| format!("写 package.json: {}", e))?;
        println!("  ✅ package.json: version -> {}", new_ver);
    } else if cargo.exists() {
        // rust 项目
        let raw = fs::read_to_string(&cargo).map_err(|e| format!("读 Cargo.toml: {}", e))?;
        let mut found = false;
        let new_raw = raw
            .lines()
            .map(|line| {
                if line.trim_start().starts_with("version = ") && !found {
                    found = true;
                    let indent = &line[..line.len() - line.trim_start().len()];
                    format!("{}version = \"{}\"", indent, new_ver)
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !found {
            return Err("Cargo.toml 未找到 version 字段".to_string());
        }
        fs::write(&cargo, new_raw + "\n").map_err(|e| format!("写 Cargo.toml: {}", e))?;
        println!("  ✅ Cargo.toml: version -> {}", new_ver);
    } else {
        return Err("未找到 package.json 或 Cargo.toml（不支持的项目类型）".to_string());
    }
    Ok(())
}

/// 追加 CHANGELOG 段
fn update_changelog(repo: &str, new_ver: &str, typ: &str, desc: &str) -> Result<(), String> {
    let changelog = Path::new(repo).join("CHANGELOG.md");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let typ_label = match typ {
        "breaking" => "破坏性变更",
        "feat" => "新功能",
        _ => "修复",
    };
    let entry = format!(
        "\n## [{}] - {}\n\n### {}\n- {}\n",
        new_ver, today, typ_label, desc
    );
    if changelog.exists() {
        let mut content = fs::read_to_string(&changelog).map_err(|e| format!("读 CHANGELOG: {}", e))?;
        // 插到第一个版本段之前（保持时间倒序：新版本在最上）
        if let Some(idx) = content.find("\n## [") {
            content.insert_str(idx, &entry);
        } else {
            content.push_str(&entry);
        }
        fs::write(&changelog, content).map_err(|e| format!("写 CHANGELOG: {}", e))?;
    } else {
        fs::write(&changelog, format!("# Changelog\n{}", entry))
            .map_err(|e| format!("创建 CHANGELOG: {}", e))?;
    }
    println!("  ✅ CHANGELOG.md: 追加 [{}] {}", new_ver, desc);
    Ok(())
}

pub fn run(args: &[String]) -> i32 {
    let mut typ = "fix".to_string();
    let mut desc = String::new();
    let mut repo = ".".to_string();
    let mut dry_run = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--type" => { i += 1; if i < args.len() { typ = args[i].clone(); } }
            "--desc" => { i += 1; if i < args.len() { desc = args[i].clone(); } }
            "--repo" => { i += 1; if i < args.len() { repo = args[i].clone(); } }
            "--dry-run" => dry_run = true,
            _ => {}
        }
        i += 1;
    }

    if desc.is_empty() {
        usage();
        return 2;
    }
    if !matches!(typ.as_str(), "fix" | "feat" | "breaking") {
        println!("错误: --type 必须是 fix/feat/breaking");
        return 2;
    }

    // 确认 git 仓库
    if !dry_run {
        if let Err(e) = run_git("git", &["rev-parse", "--is-inside-work-tree"], &repo) {
            println!("错误: {}（--repo 需指向 git 仓库）", e);
            return 1;
        }
    }

    // 读当前版本
    let pkg = Path::new(&repo).join("package.json");
    let cargo = Path::new(&repo).join("Cargo.toml");
    let current = if pkg.exists() {
        let raw = fs::read_to_string(&pkg).unwrap_or_default();
        serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|v| v["version"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "?".to_string())
    } else if cargo.exists() {
        let raw = fs::read_to_string(&cargo).unwrap_or_default();
        raw.lines()
            .find(|l| l.trim_start().starts_with("version = "))
            .and_then(|l| l.split('"').nth(1))
            .map(|s| s.to_string())
            .unwrap_or_else(|| "?".to_string())
    } else {
        println!("错误: 未找到 package.json 或 Cargo.toml");
        return 1;
    };

    let (new_ver, old_ver) = match bump(&current, &typ) {
        Ok(v) => v,
        Err(e) => { println!("错误: {}", e); return 1; }
    };

    println!("═══ dsh-tools version ═══");
    println!("  仓库: {}", repo);
    println!("  类型: {} | 版本: {} → {}", typ, old_ver, new_ver);

    if dry_run {
        println!("  [dry-run] 预览完成，未执行任何变更");
        return 0;
    }

    // 1. bump 版本文件
    if let Err(e) = bump_project(&repo, &new_ver) {
        println!("错误: {}", e);
        return 1;
    }
    // 2. 更新 CHANGELOG
    if let Err(e) = update_changelog(&repo, &new_ver, &typ, &desc) {
        println!("错误: {}", e);
        return 1;
    }
    // 3. git add + commit
    if let Err(e) = run_git("git", &["add", "-A"], &repo) {
        println!("警告: git add 失败（可能有大文件被忽略）: {}", e);
    }
    let commit_msg = format!("v{}: {}", new_ver, desc);
    if let Err(e) = run_git("git", &["commit", "-m", &commit_msg], &repo) {
        println!("警告: git commit 失败（可能无变更或无身份）: {}", e);
        return 1;
    }
    println!("  ✅ git commit: {}", commit_msg);
    // 4. tag
    let tag = format!("v{}", new_ver);
    match run_git("git", &["tag", &tag], &repo) {
        Ok(_) => println!("  ✅ git tag: {}", tag),
        Err(e) => println!("  警告: tag 已存在或失败: {}", e),
    }
    // 5. 推送（分支）
    println!("  推送提示: git push origin main && git push origin tag {}", tag);
    println!("  （若 GitHub 主站 443 被墙，用 REST API 建 ref 兜底）");
    println!("═══ 完成 ═══");
    0
}
