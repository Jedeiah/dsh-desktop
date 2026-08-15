#!/usr/bin/env bash
# DSh Desktop 一键安装 / 升级脚本
#
# 用法（自动安装/升级到最新正式版）：
#   curl -sSL https://raw.githubusercontent.com/Jedeiah/dsh-desktop/main/scripts/install.sh | bash
#
# 特性：
#   - 通过 GitHub API 动态解析最新正式版，不写死版本号，以后发版无需改本脚本
#   - 用 curl 下载（不带 com.apple.quarantine 隔离标记）→ 装完直接可用，无"损坏"提示
#   - 已运行则先退出，覆盖安装，最后自动打开
set -euo pipefail

REPO="Jedeiah/dsh-desktop"
APP_NAME="DeepSeek Harness"
APP="/Applications/${APP_NAME}.app"
ARCH_SUFFIX="$(case "$(uname -m)" in arm64) echo aarch64 ;; x86_64) echo x86_64 ;; *) echo unknown ;; esac)"

if [ "$ARCH_SUFFIX" = "unknown" ]; then
  echo "!! 不支持的架构: $(uname -m)" >&2
  exit 1
fi

echo "==> 查询最新版本（${REPO}）..."
RELEASE_JSON="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest")"
TAG="$(printf '%s' "$RELEASE_JSON" | python3 -c 'import json,sys;print(json.load(sys.stdin)["tag_name"])')"
DMG_URL="$(printf '%s' "$RELEASE_JSON" | python3 -c '
import json, sys
d = json.load(sys.stdin)
arch = sys.argv[1]
for a in d["assets"]:
    if a["name"].endswith(f".{arch}.dmg"):
        print(a["browser_download_url"])
        break
' "$ARCH_SUFFIX")"

if [ -z "${TAG:-}" ] || [ -z "${DMG_URL:-}" ]; then
  echo "!! 未找到最新版本或 ${ARCH_SUFFIX} DMG 资产（可能有发布但无该架构产物）" >&2
  exit 1
fi
echo "==> 最新版本: ${TAG}  （架构: ${ARCH_SUFFIX}）"

# 退出已运行的实例（含其 dsh 子进程），避免文件占用/双实例
if pgrep -x dsh-desktop >/dev/null 2>&1; then
  echo "==> 退出正在运行的 App..."
  pkill -x dsh-desktop 2>/dev/null || true
  sleep 2
  pkill -f "dsh/lib/bin.js" 2>/dev/null || true
fi

TMP_DIR="$(mktemp -d)"
TMP_DMG="${TMP_DIR}/DSh-${TAG}.dmg"
echo "==> 下载 DMG..."
curl -fL --progress-bar -o "$TMP_DMG" "$DMG_URL"

echo "==> 挂载..."
MOUNT_PT="$(hdiutil attach "$TMP_DMG" -nobrowse -readonly | awk -F'\t' '{print $NF}' | grep '^/Volumes/' | head -1 | xargs)"
if [ -z "$MOUNT_PT" ]; then
  echo "!! 挂载失败" >&2
  rm -rf "$TMP_DIR"
  exit 1
fi

echo "==> 安装到 /Applications..."
if [ -d "$APP" ]; then
  rm -rf "$APP"
fi
ditto "$MOUNT_PT/${APP_NAME}.app" "$APP"
hdiutil detach "$MOUNT_PT" >/dev/null 2>&1 || true

rm -rf "$TMP_DIR"

# 保险：清掉可能的隔离标记（本脚本用 curl 下载本不会带，防御性处理）
xattr -dr com.apple.quarantine "$APP" 2>/dev/null || true

echo "==> ✅ 安装完成（${TAG}）"
open "$APP"
