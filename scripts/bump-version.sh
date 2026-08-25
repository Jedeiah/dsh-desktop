#!/usr/bin/env bash
# 统一 bump 版本号——唯一入口，避免 5 处散落（Cargo.toml / Cargo.lock /
# tauri.conf.json / shell.html、modal.html 的 ?v= cache-bust）漏改。
# 用法: scripts/bump-version.sh <x.y.z>
# 发版流程: bump-version.sh → 提交 → 打 tag → 推送（release.yml 里也调用本脚本）。
set -euo pipefail
NEW="${1:?usage: bump-version.sh <x.y.z>}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
# Windows runner（windows-latest）只有 `python`，mac/linux 是 `python3`
PYTHON_BIN="$(command -v python3 || command -v python)"
[ -n "$PYTHON_BIN" ] || { echo "bump-version: 找不到 python3/python" >&2; exit 1; }
"$PYTHON_BIN" - "$NEW" <<'PY'
import json, re, sys, pathlib
v = sys.argv[1]
if not re.fullmatch(r"\d+\.\d+\.\d+", v):
    sys.exit(f"版本号格式应为 x.y.z，实际: {v}")
changed = []

# 注意：Windows runner 的 python 默认 locale 编码是 cp1252，读 UTF-8 文件（含中文
# 注释）会 UnicodeDecodeError——所有读写必须显式 encoding="utf-8"（写回同理，
# 否则会把中文写坏）。macOS/Linux 默认 utf-8，显式指定保证跨平台一致。

# 1. Cargo.toml（Rust 包版本，env!("CARGO_PKG_VERSION") 来源）
p = pathlib.Path("apps/desktop/src-tauri/Cargo.toml")
s = p.read_text(encoding="utf-8")
s2 = re.sub(r'^version = ".*"$', f'version = "{v}"', s, count=1, flags=re.M)
if s2 != s:
    p.write_text(s2, encoding="utf-8"); changed.append(str(p))

# 2. Cargo.lock —— 仅 dsh-desktop 包条目（依赖树里也有形如 0.3.34 的版本，绝不能全局替换）
p = pathlib.Path("apps/desktop/src-tauri/Cargo.lock")
s = p.read_text(encoding="utf-8")
s2 = re.sub(r'(name = "dsh-desktop"\nversion = ")[^"]*(")', rf'\g<1>{v}\g<2>', s, count=1)
if s2 != s:
    p.write_text(s2, encoding="utf-8"); changed.append(str(p))

# 3. tauri.conf.json（bundle 版本；保持与 release.yml 一致的 json 格式，避免 diff 噪音）
p = pathlib.Path("apps/desktop/src-tauri/tauri.conf.json")
raw = p.read_text(encoding="utf-8")
d = json.loads(raw)
if d.get("version") != v:
    d["version"] = v
    new_raw = json.dumps(d, indent=2, ensure_ascii=False) + ("\n" if raw.endswith("\n") else "")
    p.write_text(new_raw, encoding="utf-8"); changed.append(str(p))

# 4/5. ui 资源引用 ?v= cache-bust（WebView2 缓存 key 含 query，URL 变化强制重拉）
for f in ["apps/desktop/ui/shell.html", "apps/desktop/ui/modal.html"]:
    p = pathlib.Path(f)
    s = p.read_text(encoding="utf-8")
    s2 = re.sub(r"\?v=[0-9.]+", f"?v={v}", s)
    if s2 != s:
        p.write_text(s2, encoding="utf-8"); changed.append(str(p))

print(f"bumped to {v}")
for c in changed:
    print("  changed:", c)
if not changed:
    print("  (无变化——代码已是该版本)")
PY
