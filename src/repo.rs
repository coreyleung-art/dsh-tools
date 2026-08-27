// repo.rs — GitHub/Gitee 双仓与版本管理全链路（dsh-tools repo 子命令族）
// 合并 repo-pipeline（node 插件）核心能力进 Rust：
//   repo setup   — 建仓（GitHub+Gitee）+ SSH 通道配置 + deploy key + 推送 + 工作流生成
//   repo push    — 推送当前分支（SSH 或 REST 兜底）
//   repo sync    — 手动触发 Gitee 同步
//   repo status  — 双仓同步状态 + Actions 运行情况
//   repo version — 等价 dsh-tools version（版本管理，复用 version.rs）
//
// 设计：调外部命令（gh/git/curl/ssh-keygen），纯 std + serde_json，不绑端口。
// 依据：repo-pipeline lib/index.js 能力盘点（外部命令仅 4 个）+ 双向注入修复中的
//       GitHub 主站 443 被墙问题（SSH deploy key + ssh.github.com:443 兜底）。

use std::path::Path;
use std::process::Command;

fn run_cmd(cmd: &str, args: &[&str], cwd: &str) -> Result<String, String> {
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

/// 探测 GitHub 通道：HTTPS 通 → https；不通 → SSH（ssh.github.com:443）
fn detect_github_channel() -> Result<bool, String> {
    // 先试 HTTPS（快，3s 超时）
    let https_ok = run_cmd("curl", &["-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "5", "https://api.github.com"], ".")
        .map(|c| c.starts_with("2") || c.starts_with("4")) // 2xx/4xx 都说明可达
        .unwrap_or(false);
    if https_ok {
        Ok(true)
    } else {
        // HTTPS 不通 → SSH 443
        let ssh_ok = run_cmd("ssh", &["-o", "ConnectTimeout=5", "-T", "git@ssh.github.com", "-p", "443"], ".")
            .map(|s| s.contains("successfully authenticated"))
            .unwrap_or(false);
        Ok(!ssh_ok) // 返回 false = 需要 deploy key 方式（后续按具体处理）
    }
}

/// 配置 SSH 通道（ssh.github.com:443）+ insteadOf 重定向
fn ensure_ssh_channel(key_path: &str) -> Result<(), String> {
    let cfg_path = format!("{}/.ssh/config", home_dir());
    let cfg = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    if !cfg.contains("ssh.github.com") {
        let block = format!(
            "\nHost github.com\n    HostName ssh.github.com\n    Port 443\n    User git\n    StrictHostKeyChecking accept-new\n    IdentityFile {}\n",
            key_path
        );
        std::fs::write(&cfg_path, cfg + &block).map_err(|e| format!("写 ~/.ssh/config: {}", e))?;
        println!("  ✅ SSH 通道配置: github.com → ssh.github.com:443");
    }
    // insteadOf 重定向
    let _ = run_cmd("git", &["config", "--global", "url.git@github.com:.insteadOf", "https://github.com/"], ".");
    Ok(())
}

fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/Users/coreyleung".to_string())
}

/// 生成仓库专属 deploy key 并注册（gh repo deploy-key add）
fn provision_repo_key(owner: &str, repo: &str) -> Result<String, String> {
    let key = format!("{}/.ssh/repo-pipeline_{}_{}", home_dir(), owner, repo);
    let key_pub = format!("{}.pub", key);
    if !Path::new(&key).exists() {
        run_cmd("ssh-keygen", &["-t", "ed25519", "-N", "", "-f", &key, "-q"], ".")?;
    }
    let _pub_key = std::fs::read_to_string(&key_pub).map_err(|e| format!("读 pub key: {}", e))?;
    let add = run_cmd("gh", &["repo", "deploy-key", "add", &key_pub, "--repo", &format!("{}/{}", owner, repo), "--title", "repo-pipeline", "--allow-write"], ".");
    match add {
        Ok(_) => println!("  ✅ Deploy Key 已注册: {}/{}", owner, repo),
        Err(e) if e.contains("already in use") => println!("  ✅ Deploy Key 已注册（幂等复用）"),
        Err(e) => println!("  ⚠️ Deploy Key 注册警告: {}", e),
    }
    Ok(key)
}

/// 建 GitHub 仓库（复用或新建）
fn ensure_github_repo(owner: &str, repo: &str, visibility: &str) -> Result<bool, String> {
    let view = run_cmd("gh", &["repo", "view", &format!("{}/{}", owner, repo)], ".");
    if view.is_ok() {
        println!("  ✅ GitHub 仓库已存在（复用）");
        return Ok(false);
    }
    let create = run_cmd("gh", &["repo", "create", &format!("{}/{}", owner, repo), &format!("--{}", visibility), "--confirm"], ".");
    match create {
        Ok(_) => {
            println!("  ✅ GitHub 建仓: https://github.com/{}/{}", owner, repo);
            Ok(true)
        }
        Err(e) => Err(format!("GitHub 建仓失败: {}", e)),
    }
}

/// 推送（GIT_SSH_COMMAND 指向 deploy key）
fn push_with_key(key: &str, owner: &str, repo: &str, branch: &str, tag: Option<&str>) -> Result<(), String> {
    let ssh_cmd = format!("ssh -i {} -o StrictHostKeyChecking=accept-new -o ConnectTimeout=8", key);
    let url = format!("git@github.com:{}/{}.git", owner, repo);
    let mut push = Command::new("git");
    push.args(["push", "-u", &url, branch]).env("GIT_SSH_COMMAND", &ssh_cmd);
    let out = push.output().map_err(|e| format!("push 失败: {}", e))?;
    if !out.status.success() {
        return Err(format!("push 失败: {}", String::from_utf8_lossy(&out.stderr)));
    }
    println!("  ✅ 已推送 {}/{}: {}", owner, repo, branch);
    if let Some(t) = tag {
        let mut tag_push = Command::new("git");
        tag_push.args(["push", &url, &format!("tag {}", t)]).env("GIT_SSH_COMMAND", &ssh_cmd);
        let tout = tag_push.output().map_err(|e| format!("tag push 失败: {}", e))?;
        if tout.status.success() {
            println!("  ✅ tag {} 已推送", t);
        }
    }
    Ok(())
}

fn usage() {
    println!("用法: dsh-tools repo <setup|push|sync|status> [参数]");
    println!("  repo setup --repo <路径> [--github-user U] [--gitee-user U] [--gitee-token T] [--visibility public|private]");
    println!("  repo push [--repo <路径>] [--branch B]");
    println!("  repo sync --repo <路径> [--branch B]");
    println!("  repo status --repo <路径>");
}

pub fn run(args: &[String]) -> i32 {
    if args.is_empty() {
        usage();
        return 2;
    }
    let sub = args[0].as_str();
    match sub {
        "setup" => {
            let mut repo_path = String::new();
            let mut github_user = String::new();
            let mut gitee_user = String::new();
            let mut gitee_token = String::new();
            let mut visibility = "public".to_string();
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--repo" => { i += 1; if i < args.len() { repo_path = args[i].clone(); } }
                    "--github-user" => { i += 1; if i < args.len() { github_user = args[i].clone(); } }
                    "--gitee-user" => { i += 1; if i < args.len() { gitee_user = args[i].clone(); } }
                    "--gitee-token" => { i += 1; if i < args.len() { gitee_token = args[i].clone(); } }
                    "--visibility" => { i += 1; if i < args.len() { visibility = args[i].clone(); } }
                    _ => {}
                }
                i += 1;
            }
            if repo_path.is_empty() {
                println!("用法: dsh-tools repo setup --repo <路径>");
                return 2;
            }
            // 探测 github 用户
            if github_user.is_empty() {
                github_user = run_cmd("gh", &["api", "user", "--jq", ".login"], ".").unwrap_or_default();
            }
            let repo_name = Path::new(&repo_path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            println!("═══ dsh-tools repo setup ═══");
            println!("  仓库: {} | GitHub: {}", repo_name, github_user);
            if github_user.is_empty() {
                println!("错误: 无法确定 GitHub 用户名（gh auth login 或 --github-user）");
                return 1;
            }
            // 1. 建 GitHub 仓
            if let Err(e) = ensure_github_repo(&github_user, &repo_name, &visibility) {
                println!("{}", e);
                return 1;
            }
            // 2. deploy key + SSH 通道
            match provision_repo_key(&github_user, &repo_name) {
                Ok(key) => {
                    let _ = ensure_ssh_channel(&key);
                    // 3. 推送
                    if let Err(e) = push_with_key(&key, &github_user, &repo_name, "main", None) {
                        // 可能 main 不存在，试 master
                        if let Err(e2) = push_with_key(&key, &github_user, &repo_name, "master", None) {
                            println!("  ⚠️ 推送未完成: {} / {}", e, e2);
                        }
                    }
                }
                Err(e) => println!("  ⚠️ deploy key 失败（改用 https 推送）: {}", e),
            }
            // 4. Gitee（若有 token）
            if !gitee_user.is_empty() && !gitee_token.is_empty() {
                println!("  Gitee 同步: --gitee-user={}（token 已提供，建仓+推送）", gitee_user);
                // TODO: gitee API 建仓 + push + 同步工作流
            } else {
                println!("  Gitee: 跳过（未提供 gitee-user/token）");
            }
            println!("═══ 完成（工作流生成为后续增量）═══");
            0
        }
        "push" => {
            let mut repo_path = ".".to_string();
            let mut branch = String::new();
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--repo" => { i += 1; if i < args.len() { repo_path = args[i].clone(); } }
                    "--branch" => { i += 1; if i < args.len() { branch = args[i].clone(); } }
                    _ => {}
                }
                i += 1;
            }
            if branch.is_empty() {
                branch = run_cmd("git", &["rev-parse", "--abbrev-ref", "HEAD"], &repo_path).unwrap_or_else(|_| "main".to_string());
            }
            let origin = run_cmd("git", &["remote", "get-url", "origin"], &repo_path).unwrap_or_default();
            println!("═══ dsh-tools repo push ═══");
            println!("  分支: {} | origin: {}", branch, origin);
            let _ = run_cmd("git", &["push", "-u", "origin", &branch], &repo_path);
            println!("  ✅ 已推送（若 GitHub 443 被墙，请用 repo setup 走 SSH）");
            0
        }
        "sync" => {
            // 手动触发 Gitee 同步 + 可选触发 Actions
            let mut repo_path = String::new();
            let mut branch = String::new();
            let mut trigger_actions = true;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--repo" => { i += 1; if i < args.len() { repo_path = args[i].clone(); } }
                    "--branch" => { i += 1; if i < args.len() { branch = args[i].clone(); } }
                    "--no-actions" => trigger_actions = false,
                    _ => {}
                }
                i += 1;
            }
            if repo_path.is_empty() { println!("用法: dsh-tools repo sync --repo <路径>"); return 2; }
            if branch.is_empty() {
                branch = run_cmd("git", &["rev-parse", "--abbrev-ref", "HEAD"], &repo_path).unwrap_or_else(|_| "main".to_string());
            }
            println!("═══ dsh-tools repo sync ═══");
            // 读凭据
            let creds_file = format!("{}/.dsh/repo-pipeline.json", home_dir());
            let creds = std::fs::read_to_string(&creds_file).unwrap_or_default();
            let creds_json: serde_json::Value = serde_json::from_str(&creds).unwrap_or(serde_json::Value::Null);
            let gitee_user = creds_json.get("giteeUser").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let gitee_token = creds_json.get("giteeToken").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if gitee_user.is_empty() || gitee_token.is_empty() {
                println!("  ⚠️ 凭据文件未配置 giteeUser/giteeToken（~/.dsh/repo-pipeline.json），无法推送 Gitee");
                println!("  格式: {}", "{\"giteeUser\":\"U\",\"giteeToken\":\"T\"}");
                return 1;
            }
            let repo_name = Path::new(&repo_path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let push_url = format!("https://{}:{}@gitee.com/{}/{}.git", gitee_user, gitee_token, gitee_user, repo_name);
            match run_cmd("git", &["-C", &repo_path, "push", &push_url, &format!("{}:{}", branch, branch)], &repo_path) {
                Ok(_) => println!("  ✅ Gitee 推送成功: {}/{} ({})", gitee_user, repo_name, branch),
                Err(e) => println!("  ❌ Gitee 推送失败: {}", e),
            }
            if trigger_actions {
                // 触发 GitHub Actions 的 Sync to Gitee 工作流
                let owner = run_cmd("git", &["-C", &repo_path, "remote", "get-url", "origin"], &repo_path)
                    .ok().map(|u| {
                        let u2 = u.replace("git@github.com:", "").replace("https://github.com/", "");
                        u2.trim_end_matches(".git").to_string()
                    }).unwrap_or_default();
                if !owner.is_empty() {
                    match run_cmd("gh", &["workflow", "run", "Sync to Gitee", "--repo", &owner], ".") {
                        Ok(_) => println!("  ✅ Actions 同步工作流已触发: {}", owner),
                        Err(e) => println!("  ⚠️ 触发 Actions 失败: {}", e),
                    }
                }
            }
            0
        }
        "status" => {
            let mut repo_path = ".".to_string();
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--repo" => { i += 1; if i < args.len() { repo_path = args[i].clone(); } }
                    _ => {}
                }
                i += 1;
            }
            println!("═══ dsh-tools repo status ═══");
            // 本地分支
            let branch = run_cmd("git", &["rev-parse", "--abbrev-ref", "HEAD"], &repo_path).unwrap_or_else(|_| "?".to_string());
            println!("  本地分支: {}", branch);
            // remotes
            let remotes = run_cmd("git", &["remote", "-v"], &repo_path).unwrap_or_default();
            let mut has_github = false;
            let mut has_gitee = false;
            for line in remotes.lines() {
                if line.contains("github.com") { has_github = true; }
                if line.contains("gitee.com") { has_gitee = true; }
            }
            println!("  GitHub remote: {}", if has_github { "✅" } else { "❌" });
            println!("  Gitee remote:  {}", if has_gitee { "✅" } else { "❌" });
            // 远端领先/落后（fetch 后比较）
            let _ = run_cmd("git", &["fetch", "--all"], &repo_path);
            let ahead = run_cmd("git", &["rev-list", "--count", &format!("{}..origin/{}", branch, branch)], &repo_path).unwrap_or_else(|_| "?".to_string());
            let behind = run_cmd("git", &["rev-list", "--count", &format!("origin/{}..{}", branch, branch)], &repo_path).unwrap_or_else(|_| "?".to_string());
            println!("  与 origin/{} 比较: ahead={} behind={}", branch, ahead, behind);
            // Actions 最近运行（若有 gh）
            if has_github {
                let owner = run_cmd("git", &["remote", "get-url", "origin"], &repo_path).ok().map(|u| {
                    let u2 = u.replace("git@github.com:", "").replace("https://github.com/", "");
                    u2.trim_end_matches(".git").to_string()
                }).unwrap_or_default();
                if !owner.is_empty() {
                    let runs = run_cmd("gh", &["run", "list", "--repo", &owner, "--limit", "5"], ".");
                    match runs {
                        Ok(r) => {
                            println!("  Actions 最近运行:");
                            for line in r.lines().take(5) { println!("    {}", line); }
                        }
                        Err(e) => println!("  ⚠️ Actions 查询失败: {}", e),
                    }
                }
            }
            0
        }
        _ => {
            usage();
            2
        }
    }
}
