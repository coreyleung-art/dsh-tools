#!/usr/bin/env python3
"""rest-push.py — 用 GitHub REST API 完整推送（github.com:443 被墙时替代 git push）
用法: python3 rest-push.py <repo> [--tag vX.Y.Z]
流程: blobs → tree → commit → refs/heads/main → refs/tags/<tag>
"""
import json, sys, base64, subprocess, urllib.request, os

REPO = sys.argv[1] if len(sys.argv) > 1 else None
TAG = None
for i, a in enumerate(sys.argv):
    if a == "--tag" and i+1 < len(sys.argv):
        TAG = sys.argv[i+1]
if not REPO:
    print("用法: rest-push.py <owner/repo> [--tag vX.Y.Z]")
    sys.exit(1)

TOKEN = os.popen("gh auth token").read().strip()
BASE = f"https://api.github.com/repos/{REPO}"

def api(method, path, data=None):
    url = BASE + path
    body = json.dumps(data).encode() if data else None
    req = urllib.request.Request(url, data=body, method=method)
    req.add_header("Authorization", f"token {TOKEN}")
    req.add_header("Accept", "application/vnd.github+json")
    if data: req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        err = e.read().decode()[:200]
        print(f"  HTTP {e.code} {path}: {err}")
        return None

def git(files):
    # 1. blobs
    blob_map = {}
    for f in files:
        with open(f, "rb") as fh:
            content = base64.b64encode(fh.read()).decode()
        r = api("POST", "/git/blobs", {"content": content, "encoding": "base64"})
        if r: blob_map[f] = r["sha"]
        print(f"  blob {f} -> {blob_map.get(f, 'FAIL')[:10]}...")
    # 2. tree
    tree_items = []
    for f, sha in blob_map.items():
        tree_items.append({"path": f, "mode": "100644", "type": "blob", "sha": sha})
    tree = api("POST", "/git/trees", {"tree": tree_items})
    if not tree: print("  tree FAIL"); return False
    print(f"  tree {tree['sha'][:10]}...")
    # 3. commit
    msg = "dsh-tools v1.5.0: 版本管理初始化 + version 子命令"
    commit = api("POST", "/git/commits", {"message": msg, "tree": tree["sha"], "parents": []})
    if not commit: print("  commit FAIL"); return False
    print(f"  commit {commit['sha'][:10]}...")
    # 4. ref main
    ref = api("POST", "/git/refs", {"ref": "refs/heads/main", "sha": commit["sha"]})
    if not ref: print("  ref main FAIL（可能已存在）"); return False
    print(f"  ✅ refs/heads/main -> {commit['sha'][:10]}...")
    # 5. tag
    if TAG:
        t = api("POST", "/git/refs", {"ref": f"refs/tags/{TAG}", "sha": commit["sha"]})
        print(f"  {'✅' if t else '❌'} refs/tags/{TAG}")
    return True

if __name__ == "__main__":
    # 获取文件列表（git ls-files）
    r = subprocess.run(["git", "ls-files"], capture_output=True, text=True, cwd=".")
    files = [l for l in r.stdout.strip().split("\n") if l and os.path.isfile(l)]
    print(f"═══ REST 推送 {REPO}（{len(files)} 文件）═══")
    ok = git(files)
    print("═══", "✅ 完成" if ok else "❌ 失败", "═══")
    sys.exit(0 if ok else 1)
