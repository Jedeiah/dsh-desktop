#!/usr/bin/env bash
# DSh Desktop 一键安装 / 升级脚本
#
# 用法（自动安装/升级到最新正式版）：
#   curl -sSL https://raw.githubusercontent.com/Jedeiah/dsh-desktop/main/scripts/install.sh | bash
#
# 特性：
#   - 通过 GitHub API 动态解析最新正式版，不写死版本号，以后发版无需改本脚本
#   - 用 curl 下载（不带 com.apple.quarantine 隔离标记）→ 装完直接可用，无"损坏"提示
#   - 已运行则先退出（只杀本 App 自己的进程，不动终端里你手动跑的 dsh），覆盖安装后自动打开
#   - 任何退出路径都自动清理：卸载挂载 + 删除下载的 DMG/临时目录（trap）
set -euo pipefail

REPO="Jedeiah/dsh-desktop"
APP_NAME="DeepSeek Harness Desktop"
APP="/Applications/${APP_NAME}.app"
# App 自己子进程的特征串（区别于用户终端里手动跑的 dsh：App 用内置 node
# 运行 app-data 闭包的 bin.js --profile web；终端 dsh 的命令行不含该特征）
DSH_CHILD_PATTERN="node_modules/@deepseek-ai/dsh/lib/bin.js --profile web"

case "$(uname -m)" in
  arm64)  ARCH_SUFFIX="aarch64" ;;
  x86_64)
    # CI 只构建 Apple Silicon 产物（macOS runner = macos-14/arm64），没有 x86_64 DMG：
    # 继续走下去只会 curl 到 404 报错，不如在这里说清原因。
    echo "!! 暂不支持 Intel（x86_64）Mac：目前只发布 Apple Silicon（arm64）版本。" >&2
    echo "   可用 Apple Silicon 机器安装，或在 Intel 机上自行从源码构建：" >&2
    echo "   https://github.com/${REPO}#开发" >&2
    exit 1 ;;
  *) echo "!! 不支持的架构: $(uname -m)" >&2; exit 1 ;;
esac

echo "==> 查询最新版本（${REPO}）..."
# 走 github.com 的 /releases/latest 跳转拿最新 tag（不依赖 api.github.com，
# 后者在国内网络可能慢/被墙导致挂起）；全部 curl 带连接超时，绝不无限等。
TAG_URL="$(curl -sL --connect-timeout 10 --max-time 25 -o /dev/null -w '%{url_effective}' \
  "https://github.com/${REPO}/releases/latest" 2>/dev/null || true)"
TAG="${TAG_URL##*/tag/}"

if [ -z "$TAG" ] || [ "$TAG" = "$TAG_URL" ]; then
  echo "!! 未找到最新版本（可能尚无发布）" >&2
  exit 1
fi

# 资产名：release 工作流产物为 DeepSeek Harness Desktop_<ver>_<arch>.dmg，
# GitHub 上传时把空格替换为点号 → 用固定命名拼下载地址（绕开 API）。
VERSION="${TAG#v}"
DMG_URL="https://github.com/${REPO}/releases/download/${TAG}/DeepSeek.Harness.Desktop_${VERSION}_${ARCH_SUFFIX}.dmg"

echo "==> 最新版本: ${TAG}  （架构: ${ARCH_SUFFIX}）"

# 退出已运行的实例：先温和 SIGTERM，轮询等待退出；再精确清掉 App 自己的子进程
# （不匹配终端里用户手动跑的 dsh，避免误杀）
if pgrep -x dsh-desktop >/dev/null 2>&1; then
  echo "==> 退出正在运行的 App..."
  pkill -x dsh-desktop 2>/dev/null || true
  for _ in $(seq 1 15); do
    pgrep -x dsh-desktop >/dev/null 2>&1 || break
    sleep 1
  done
  pkill -x dsh-desktop 2>/dev/null || true # 15s 未退再补一刀（SIGTERM）
  pkill -f "$DSH_CHILD_PATTERN" 2>/dev/null || true
  sleep 1
fi

TMP_DIR="$(mktemp -d)"
MOUNT_PT=""
# 任何退出路径（成功或失败）都清理：卸载挂载 + 删除临时目录（含下载的 DMG）
# 注：detach 后必须 `|| true`。安装成功路径已把 MOUNT_PT 置空，此处 detach 必失败；
# 配合 set -e，trap 第一条命令失败会中断整个 trap，导致 rm -rf 永远执行不到、临时目录漏出。
trap 'hdiutil detach "$MOUNT_PT" >/dev/null 2>&1 || true; rm -rf "$TMP_DIR"' EXIT
TMP_DMG="${TMP_DIR}/DSh-${TAG}.dmg"

echo "==> 下载 DMG..."
curl -fL --max-time 600 --progress-bar -o "$TMP_DMG" "$DMG_URL"

# 校验和：release 为每个产物同时发布 <asset>.sha256（App 内自动更新也校验它）。
# 一键脚本不校验就等于把"整条安装链的最后一环"交给网络——静默装到损坏/被替换的包。
# 只用校验和文件里的哈希（不比文件名：文件里记的是 CI 侧的路径）。
echo "==> 校验下载完整性..."
EXPECT="$(curl -fsL --connect-timeout 10 --max-time 30 "${DMG_URL}.sha256" 2>/dev/null \
  | awk '{print $1}' | tr -d '[:space:]' | head -c 64 || true)"
if [ -z "$EXPECT" ]; then
  echo "!! 无法获取校验和（${DMG_URL}.sha256）" >&2
  exit 1
fi
ACTUAL="$(shasum -a 256 "$TMP_DMG" | awk '{print $1}')"
if [ "$EXPECT" != "$ACTUAL" ]; then
  echo "!! 校验失败：下载内容与发布校验和不一致" >&2
  echo "   期望 $EXPECT" >&2
  echo "   实际 $ACTUAL" >&2
  exit 1
fi
echo "    ✓ sha256 一致"

echo "==> 挂载..."
MOUNT_PT="$(hdiutil attach "$TMP_DMG" -nobrowse -readonly \
  | awk -F'\t' '{print $NF}' | grep '^/Volumes/' | head -1 \
  | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
if [ -z "$MOUNT_PT" ]; then
  echo "!! 挂载失败（DMG 可能损坏或不是磁盘镜像）" >&2
  exit 1
fi

echo "==> 安装到 /Applications..."
if [ -d "$APP" ]; then
  rm -rf "$APP"
fi
ditto "$MOUNT_PT/${APP_NAME}.app" "$APP"
hdiutil detach "$MOUNT_PT" >/dev/null 2>&1 || true
MOUNT_PT=""

# 保险：清掉可能的隔离标记（本脚本用 curl 下载本不会带，防御性处理）
xattr -dr com.apple.quarantine "$APP" 2>/dev/null || true

echo "==> ✅ 安装完成（${TAG}）"
open "$APP"
