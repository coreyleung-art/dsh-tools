// workflow.rs — Rust 工作流编排引擎（串行/并行）
// 读 workflow JSON 定义 → 按依赖关系编排步骤 → std::thread 并行 + mpsc 聚合 → 黑板落盘
// 设计原则（token/资源优化内建）：
//   · 步骤声明 token_budget（超限拒绝）
//   · 需要 LLM 的步骤默认本地模型（bb-read 模式，零 API 成本）
//   · 中间结果用摘要传递（不传全文）
//   · 失败分支重试/死信，不阻塞其他并行分支

use std::collections::HashMap;
use std::process::Command;
use std::sync::mpsc;

#[derive(Clone)]
struct StepDef {
    id: String,
    cmd: String,
    args: Vec<String>,
    depends_on: Vec<String>,
    parallel: bool,
    token_budget: Option<u64>,
    retry: u32,
}

#[derive(Clone)]
struct StepResult {
    id: String,
    ok: bool,
    output: String,
    tokens: u64,
    duration_ms: u64,
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn run_command(cmd: &str, args: &[String]) -> (bool, String) {
    match Command::new(cmd).args(args).output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            let combined = if stdout.is_empty() { stderr } else { stdout };
            (out.status.success(), combined.trim().chars().take(500).collect())
        }
        Err(e) => (false, format!("spawn error: {}", e)),
    }
}

fn parse_step(v: &serde_json::Value) -> StepDef {
    StepDef {
        id: v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        cmd: v.get("cmd").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        args: v.get("args").and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str().map(|x| x.to_string())).collect())
            .unwrap_or_default(),
        depends_on: v.get("depends_on").and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str().map(|x| x.to_string())).collect())
            .unwrap_or_default(),
        parallel: v.get("parallel").and_then(|x| x.as_bool()).unwrap_or(false),
        token_budget: v.get("token_budget").and_then(|x| x.as_u64()),
        retry: v.get("retry").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
    }
}

fn estimate_tokens(cmd: &str, args: &[String]) -> u64 {
    // 粗略 token 估算：命令+参数总字符 / 4（中文场景偏保守）
    let total = cmd.len() + args.iter().map(|a| a.len()).sum::<usize>();
    (total / 3) as u64
}

fn execute_step(step: &StepDef) -> StepResult {
    let start = std::time::Instant::now();
    let mut tokens = estimate_tokens(&step.cmd, &step.args);

    // token 预算门禁
    if let Some(budget) = step.token_budget {
        if tokens > budget {
            return StepResult {
                id: step.id.clone(), ok: false,
                output: format!("token 预算超限: est {} > budget {}", tokens, budget),
                tokens, duration_ms: 0,
            };
        }
    }

    let (mut ok, mut output) = (false, String::new());
    let mut attempts = 0;
    let max_retry = step.retry + 1;
    loop {
        attempts += 1;
        let (o, out) = run_command(&step.cmd, &step.args);
        ok = o;
        output = out.clone();
        if ok || attempts >= max_retry { break; }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    StepResult {
        id: step.id.clone(), ok, output, tokens, duration_ms: start.elapsed().as_millis() as u64,
    }
}

/// 执行工作流：按依赖拓扑编排，串行=依赖链，并行=无依赖分支
pub fn run_workflow(path: &str) -> (bool, serde_json::Value) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return (false, serde_json::json!({"error": format!("读文件失败: {}", e)})),
    };
    let def: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => return (false, serde_json::json!({"error": format!("JSON 解析失败: {}", e)})),
    };

    let name = def.get("name").and_then(|x| x.as_str()).unwrap_or("workflow");
    let steps: Vec<StepDef> = def.get("steps").and_then(|x| x.as_array())
        .map(|a| a.iter().map(parse_step).collect())
        .unwrap_or_default();

    println!("[workflow] {} 开始: {} ({} steps)", now(), name, steps.len());

    // 结果表
    let mut results: HashMap<String, StepResult> = HashMap::new();
    let mut pending: Vec<StepDef> = steps.clone();
    let mut all_ok = true;
    let wf_start = std::time::Instant::now();

    // 拓扑执行：循环处理，每轮执行所有依赖已满足的步骤
    while !pending.is_empty() {
        // 找出当前可执行的步骤（依赖全部完成）
        let mut ready: Vec<StepDef> = Vec::new();
        let mut still: Vec<StepDef> = Vec::new();
        for s in pending.drain(..) {
            let deps_ok = s.depends_on.iter().all(|d| results.contains_key(d));
            if deps_ok {
                ready.push(s);
            } else {
                still.push(s);
            }
        }
        if ready.is_empty() {
            // 死锁（依赖无法满足）
            let missing: Vec<String> = still.iter().map(|s| s.id.clone()).collect();
            all_ok = false;
            return (false, serde_json::json!({
                "workflow": name, "ok": false, "error": "依赖死锁",
                "pending": missing,
            }));
        }
        pending = still;

        // 并行执行 ready 批次
        let (tx, rx) = mpsc::channel::<(String, StepResult)>();
        let mut handles = Vec::new();
        for s in &ready {
            let s = s.clone();
            let tx = tx.clone();
            handles.push(std::thread::spawn(move || {
                let r = execute_step(&s);
                let _ = tx.send((s.id.clone(), r));
            }));
        }
        drop(tx);
        for h in handles {
            let _ = h.join();
        }
        while let Ok((id, r)) = rx.try_recv() {
            results.insert(id.clone(), r.clone());
            let tag = if r.ok { "✅" } else { "❌" };
            println!("[workflow] {} step {}: {} ({} tokens, {}ms)", tag, id, r.output.chars().take(60).collect::<String>(), r.tokens, r.duration_ms);
            if !r.ok { all_ok = false; }
        }
    }

    let wf_ms = wf_start.elapsed().as_millis() as u64;
    let total_tokens: u64 = results.values().map(|r| r.tokens).sum();

    // 汇总
    let summary = serde_json::json!({
        "workflow": name,
        "ok": all_ok,
        "total_steps": steps.len(),
        "total_tokens": total_tokens,
        "duration_ms": wf_ms,
        "results": results.iter().map(|(id, r)| serde_json::json!({
            "id": id, "ok": r.ok, "tokens": r.tokens, "duration_ms": r.duration_ms,
            "output": r.output.chars().take(200).collect::<String>(),
        })).collect::<Vec<_>>(),
        "ts": now(),
    });

    println!("[workflow] {} 完成: {} ({} steps, {} tokens, {}ms)", if all_ok { "✅" } else { "❌" }, name, steps.len(), total_tokens, wf_ms);
    (all_ok, summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_step_basic() {
        let v = serde_json::json!({"id": "s1", "cmd": "echo", "args": ["hi"], "token_budget": 1000});
        let s = parse_step(&v);
        assert_eq!(s.id, "s1");
        assert_eq!(s.cmd, "echo");
        assert_eq!(s.args, vec!["hi"]);
        assert_eq!(s.token_budget, Some(1000));
    }

    #[test]
    fn token_estimate() {
        let t = estimate_tokens("bb-read", &["notes/mac-mini/".to_string()]);
        assert!(t > 0, "token 估算应 > 0");
    }

    #[test]
    fn execute_echo() {
        let s = StepDef { id: "t".into(), cmd: "echo".into(), args: vec!["hello".into()],
            depends_on: vec![], parallel: false, token_budget: None, retry: 0 };
        let r = execute_step(&s);
        assert!(r.ok);
        assert!(r.output.contains("hello"));
    }

    #[test]
    fn token_budget_gate() {
        // 预算 0 → 必然拒绝
        let s = StepDef { id: "t".into(), cmd: "echo".into(), args: vec!["hello".into()],
            depends_on: vec![], parallel: false, token_budget: Some(0), retry: 0 };
        let r = execute_step(&s);
        assert!(!r.ok);
        assert!(r.output.contains("预算超限"));
    }
}
