#!/usr/bin/env bash
# Prepare bundled resources for DSh Desktop (M1):
#   resources/node/bin/node  — Node arm64 runtime (copied from fnm install)
#   resources/dsh/<ver>/     — full dsh closure (node_modules incl. @deepseek-ai/dsh)
#   resources/dsh/current    — symlink to the active version
#   icons/icon.png + icon.icns — app/tray icons (generated locally)
#
# Build-time script: the machine running this may use npm/fnm; the bundled
# result is what matters at runtime (no system bun/npm/node needed there).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC_TAURI="$ROOT/apps/desktop/src-tauri"
RES="$SRC_TAURI/resources"
VER="${DSH_VERSION:-0.1.0-rc.6}"

echo "==> Preparing resources for dsh closure $VER"

# --- 1. Node runtime ---------------------------------------------------------
NODE_SRC="${NODE_SRC:-$HOME/.local/share/fnm/node-versions/v24.14.0/installation/bin/node}"
if [[ ! -x "$NODE_SRC" ]]; then
  echo "node source not found at $NODE_SRC; set NODE_SRC to a node binary" >&2
  exit 1
fi
mkdir -p "$RES/node/bin"
cp -f "$NODE_SRC" "$RES/node/bin/node"
echo "    node: $( "$RES/node/bin/node" --version )"

# --- 2. dsh closure ---------------------------------------------------------
# Source: default to the currently bundled closure (project-local, self-sufficient);
# override with CLOSURE_SRC=<dir containing node_modules/@deepseek-ai/dsh> for a new version.
if [[ -z "${CLOSURE_SRC:-}" && -d "$RES/dsh/current/node_modules" ]]; then
  CLOSURE_SRC="$RES/dsh/current/node_modules"
fi
if [[ ! -d "$CLOSURE_SRC/@deepseek-ai/dsh" ]]; then
  echo "closure source missing @deepseek-ai/dsh at $CLOSURE_SRC; set CLOSURE_SRC" >&2
  exit 1
fi
# stage first: the source may live inside the dir we are about to rewrite
# (e.g. resources/dsh/current), so never delete it before copying.
STAGE="$(mktemp -d)"
cp -R "$CLOSURE_SRC" "$STAGE/node_modules"
DST="$RES/dsh/$VER"
rm -rf "$DST"
mkdir -p "$DST"
mv "$STAGE/node_modules" "$DST/node_modules"
rmdir "$STAGE"
echo "{\"name\":\"dsh-closure\",\"version\":\"$VER\"}" >"$DST/package.json"
echo "$VER" >"$DST/VERSION"
# `current` 是版本标记文件（内容=版本号），跨平台（Windows 无软链权限也通用）。
# 旧版用软链时 Rust 侧会回退扫描，无需迁移。
echo "$VER" >"$RES/dsh/current"
rm -f "$RES/dsh/current.tmp"
echo "    closure: $(du -sh "$DST" | cut -f1) at $DST"

# --- 2b. LAN proxy (M6: 局域网访问转发器) --------------------------------
cp "$SRC_TAURI/lan-proxy.js" "$RES/lan-proxy.js"
echo "    lan-proxy.js: $(wc -c < "$RES/lan-proxy.js" | tr -d ' ')B"
# --- 2b2. mDNS 通告器（壳层增强②，Windows 用） ---------------------------
cp "$SRC_TAURI/mdns-advertise.js" "$RES/mdns-advertise.js"
echo "    mdns-advertise.js: $(wc -c < "$RES/mdns-advertise.js" | tr -d ' ')B"
# 无网络自测：验证 mDNS 报文逻辑（打包前兜底，失败即中止）
if ! "$RES/node/bin/node" "$RES/mdns-advertise.js" --self-test >/dev/null 2>&1; then
  echo "ERROR: mdns-advertise self-test failed" >&2
  exit 1
fi
echo "    mdns-advertise.js self-test OK"

# --- 2b3. pnpm 独立二进制（插件管理；@pnpm/exe 平台二进制，零运行时网络）---
# dsh plugin 写死 spawnSync("pnpm") 从 PATH 找；App 内置该二进制并注入 PATH，
# 用户无需自行安装 pnpm。@pnpm/exe 通过 optionalDependencies 装当前平台版本；
# 钉 ^11（pnpm 11 的门禁语义：allowBuilds / minimumReleaseAge，pnpm 12 为
# Rust 重写，行为未验证）。注意：@pnpm/exe 目前不发布 Intel macOS 二进制
# （Node SEA 上游问题），Intel 机器上安装会失败——拷出后必须校验可执行，
# 失败即中止，避免把占位符文本当二进制打包进 App。
PNPM_STAGE="$(mktemp -d)"
if npm install --prefix "$PNPM_STAGE" "@pnpm/exe@^11" --no-audit --no-fund >/dev/null 2>&1; then
  mkdir -p "$RES/pnpm-bin"
  if [[ -f "$PNPM_STAGE/node_modules/@pnpm/exe/pnpm" ]]; then
    cp -f "$PNPM_STAGE/node_modules/@pnpm/exe/pnpm" "$RES/pnpm-bin/pnpm"
    chmod +x "$RES/pnpm-bin/pnpm"
  elif [[ -f "$PNPM_STAGE/node_modules/@pnpm/exe/pnpm.exe" ]]; then
    cp -f "$PNPM_STAGE/node_modules/@pnpm/exe/pnpm.exe" "$RES/pnpm-bin/pnpm.exe"
  else
    echo "ERROR: @pnpm/exe 二进制未找到（本平台不支持？）" >&2
    exit 1
  fi
  # 校验可执行（Intel Mac 的 setup.js 会失败并留下占位符文本）
  BIN="$RES/pnpm-bin/pnpm$([ -f "$RES/pnpm-bin/pnpm.exe" ] && echo .exe)"
  PNPM_VER="$("$BIN" --version 2>/dev/null || true)"
  if [[ -z "$PNPM_VER" ]]; then
    echo "ERROR: 内置 pnpm 校验失败（$BIN 不可执行；本机为 Intel Mac？@pnpm/exe 暂不支持）" >&2
    exit 1
  fi
  echo "    pnpm: $PNPM_VER"
else
  echo "ERROR: @pnpm/exe 安装失败" >&2
  exit 1
fi
rm -rf "$PNPM_STAGE"

# --- 2c. closure self-check (must boot under the bundled node) --------------
DETECTED_VER="$("$RES/node/bin/node" "$DST/node_modules/@deepseek-ai/dsh/lib/bin.js" --version 2>/dev/null || true)"
if [[ "$DETECTED_VER" != "$VER" ]]; then
  echo "ERROR: bundled closure does not boot (expected version $VER, got '${DETECTED_VER}')" >&2
  exit 1
fi
echo "    closure self-check OK: dsh $DETECTED_VER"

# --- 3. icons ----------------------------------------------------------------
# The app icon is the user-provided one: icons/icon.png (RGBA 1024) + icon.icns.
# If either is missing, regenerate from a source image (env ICON_SRC).
ICONS="$SRC_TAURI/icons"
mkdir -p "$ICONS"
if [[ ! -f "$ICONS/icon.png" || ! -f "$ICONS/icon.icns" ]]; then
  ICON_SRC="${ICON_SRC:-$HOME/Downloads/dsh.png}"
  if [[ ! -f "$ICON_SRC" ]]; then
    echo "missing app icon; put icon.png (RGBA 1024) in $ICONS or set ICON_SRC" >&2
    exit 1
  fi
  echo "    regenerating icons from $ICON_SRC"
  sips -z 1024 1024 -s format png "$ICON_SRC" --out "$ICONS/icon-1024.png" >/dev/null
  # RGB -> RGBA (pure python, no PIL)
  python3 - "$ICONS/icon-1024.png" "$ICONS/icon.png" <<'PY'
import struct, sys, zlib

def read_png(path):
    data = open(path,'rb').read()
    assert data[:8] == b'\x89PNG\r\n\x1a\n'
    pos = 8; idat = b''; w=h=bitd=ct=0
    while pos < len(data):
        ln = struct.unpack('>I', data[pos:pos+4])[0]
        tag = data[pos+4:pos+8]; chunk = data[pos+8:pos+8+ln]
        if tag == b'IHDR': w,h,bitd,ct,_,_,_ = struct.unpack('>IIBBBBB', chunk)
        elif tag == b'IDAT': idat += chunk
        pos += 12 + ln
    assert bitd == 8 and ct in (2,6)
    return w,h,ct,zlib.decompress(idat)

def unfilter(w,h,bpp,raw):
    stride = w*bpp; out = bytearray(); prev = bytearray(stride); p = 0
    for y in range(h):
        f = raw[p]; p += 1
        line = bytearray(raw[p:p+stride]); p += stride
        for i in range(stride):
            a = line[i-bpp] if i>=bpp else 0
            b = prev[i]
            c = prev[i-bpp] if i>=bpp else 0
            if f==1: line[i]=(line[i]+a)&255
            elif f==2: line[i]=(line[i]+b)&255
            elif f==3: line[i]=(line[i]+((a+b)//2))&255
            elif f==4:
                pp=a+b-c; pa=abs(pp-a); pb=abs(pp-b); pc=abs(pp-c)
                pr = a if (pa<=pb and pa<=pc) else (b if pb<=pc else c)
                line[i]=(line[i]+pr)&255
        out += line; prev = line
    return out

def write_png(path,w,h,rows):
    def chunk(tag,d):
        c=tag+d
        return struct.pack('>I',len(d))+c+struct.pack('>I',zlib.crc32(c)&0xffffffff)
    ihdr=struct.pack('>IIBBBBB',w,h,8,6,0,0,0)
    raw=b''.join(b'\x00'+bytes(r) for r in rows)
    open(path,'wb').write(b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',ihdr)+chunk(b'IDAT',zlib.compress(raw,9))+chunk(b'IEND',b''))

w,h,ct,raw = read_png(sys.argv[1])
bpp = 3 if ct==2 else 4
px = unfilter(w,h,bpp,raw)
rows=[]
for y in range(h):
    row = px[y*w*bpp:(y+1)*w*bpp]
    if ct==2:
        # interleave R,G,B and append alpha=255 (do NOT drop channels!)
        rgba = bytearray()
        for i in range(0,len(row),3):
            rgba += bytes((row[i],row[i+1],row[i+2],255))
        rows.append(bytes(rgba))
    else:
        rows.append(bytes(row))
write_png(sys.argv[2],w,h,rows)
print("    icon.png (RGBA) regenerated")
PY
  rm -f "$ICONS/icon-1024.png"
  ISET="$ICONS/icon.iconset"
  rm -rf "$ISET"; mkdir -p "$ISET"
  for s in 16 32 64 128 256 512 1024; do
    sips -z "$s" "$s" "$ICONS/icon.png" --out "$ISET/icon_${s}x${s}.png" >/dev/null
  done
  for s in 16 32 64 128 256 512; do
    d=$((s*2))
    sips -z "$d" "$d" "$ICONS/icon.png" --out "$ISET/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$ISET" -o "$ICONS/icon.icns"
  rm -rf "$ISET"
  echo "    icon.icns regenerated"
fi

echo "==> Done. resources:"
du -sh "$RES"
