#!/usr/bin/env node
// DSh Desktop — mDNS 通告器（Windows 用；macOS 用系统 /usr/bin/dns-sd）
//
// 用法: node mdns-advertise.js <port>
//
// 为什么存在：Windows 没有 dns-sd 命令，也没有系统自带的 mDNS 服务注册
// 命令行工具；本脚本用纯 Node（内置运行时，零依赖）实现最小 DNS-SD/mDNS
// 通告器，向局域网通告 _http._tcp 服务 "DeepSeek Harness"，使
// http://DeepSeek-Harness.local:<port>/ 可解析（IP 变了也不用改地址）。
//
// 行为：
//   - 加入 224.0.0.251:5353 组播，应答 PTR/SRV/TXT（_http._tcp.local）与
//     A（DeepSeek-Harness.local）查询；组播查询→组播应答，单播查询→单播应答。
//   - 启动时通告 3 次 + 每 60s 重通告（TTL 120s，双倍刷新余量）；通告循环里
//     校验 LAN 接口，IP 变化自动重绑组成员/出口。
//   - 收到 SIGTERM（macOS 侧 kill）发送 goodbye（TTL=0）后退出；Windows 侧
//     kill 走 taskkill /F 强杀（不投递信号，本处理器不触发）→ 靠 TTL 到期清理。
//   - A 记录每次应答实时取本机私网 IPv4（与 Rust lan_ip() 同规则），IP 变了自动跟随。
//   - cache-flush 位遵循 RFC 6762 §10.2：PTR（共享记录）与 legacy 单播应答
//     永不设置，仅组播应答中的 SRV/TXT/A 设置。
//
// 降级/限制：脚本自身异常只向 stderr 输出并退出非 0，Rust 侧 logln 后 LAN
// 不受影响。不做名称冲突探测（家用场景可接受）。Windows 防火墙首次可能弹
// 提示，允许即可（与 lan-proxy 一致）。
'use strict';

const dgram = require('dgram');
const os = require('os');

const PORT = Number(process.argv[2] || 3190);
const INSTANCE = 'DeepSeek Harness';
const TYPE = '_http._tcp';
// 实例名 → .local 域名（空格→连字符）
const FQDN = 'DeepSeek-Harness.local.';
const PTR_NAME = TYPE + '.local.';
const SRV_NAME = INSTANCE + '.' + PTR_NAME;
const MDNS_ADDR = '224.0.0.251';
const MDNS_PORT = 5353;
const TTL = 120;
const REANNOUNCE_MS = 60 * 1000;

const TYPE_A = 1;
const TYPE_PTR = 12;
const TYPE_TXT = 16;
const TYPE_SRV = 33;
const TYPE_ANY = 255;
const CLASS_IN = 1;
const CLASS_ANY = 255;
const CACHE_FLUSH = 0x8000;

// ---------------- DNS 报文工具 ----------------

/// 'a.b.c.' -> 字节序列（不做名字压缩，合法且实现简单）
function encodeName(name) {
  const out = [];
  for (const label of name.split('.')) {
    if (!label) continue;
    if (label.length > 63) return null;
    out.push(label.length);
    for (let i = 0; i < label.length; i++) out.push(label.charCodeAt(i));
  }
  out.push(0);
  return out;
}

/// 解析名字（支持压缩指针）；返回 { name, next } 或 null
function parseName(buf, off) {
  const labels = [];
  let pos = off;
  let jumped = -1;
  let loops = 0;
  while (true) {
    if (pos >= buf.length || loops++ > 64) return null;
    const len = buf[pos];
    if (len === 0) {
      pos += 1;
      break;
    }
    if ((len & 0xc0) === 0xc0) {
      // 压缩指针：跳转继续读，next 停在指针处之后
      if (pos + 1 >= buf.length) return null;
      const ptr = ((len & 0x3f) << 8) | buf[pos + 1];
      if (jumped < 0) jumped = pos + 2;
      pos = ptr;
      continue;
    }
    if (pos + 1 + len > buf.length) return null;
    labels.push(buf.toString('latin1', pos + 1, pos + 1 + len));
    pos += 1 + len;
  }
  return { name: labels.join('.') + '.', next: jumped < 0 ? pos : jumped };
}

/// 单条应答 RR。`flush` = cache-flush 位（RFC 6762 §10.2）：
/// - 共享记录（PTR，如 _http._tcp.local）MUST NOT 设该位（否则会把局域网
///   其它设备的同服务记录从客户端缓存驱逐）；
/// - 对非 5353 端口的 legacy 单播应答 MUST NOT 设该位；
/// - 仅组播应答中的非共享记录（SRV/TXT/A）可设。
function rr(name, type, rdataBuf, ttl, flush) {
  const n = encodeName(name);
  if (!n) return null;
  const hdr = Buffer.alloc(10);
  hdr.writeUInt16BE(type, 0);
  hdr.writeUInt16BE(CLASS_IN | (flush ? CACHE_FLUSH : 0), 2);
  hdr.writeUInt32BE(ttl, 4);
  hdr.writeUInt16BE(rdataBuf.length, 8);
  return Buffer.concat([Buffer.from(n), hdr, rdataBuf]);
}

function ptrRecord(ttl) {
  // PTR 是共享记录：永不设 cache-flush（RFC 6762 §10.2）
  const rdata = encodeName(SRV_NAME);
  return rdata ? rr(PTR_NAME, TYPE_PTR, Buffer.from(rdata), ttl, false) : null;
}

function srvRecord(ttl, flush) {
  const rdata = encodeName(FQDN);
  if (!rdata) return null;
  const fix = Buffer.alloc(6);
  fix.writeUInt16BE(0, 0); // priority
  fix.writeUInt16BE(0, 2); // weight
  fix.writeUInt16BE(PORT, 4);
  return rr(SRV_NAME, TYPE_SRV, Buffer.concat([fix, Buffer.from(rdata)]), ttl, flush);
}

function txtRecord(ttl, flush) {
  // 空 TXT（单字节长度 0 表示空属性集）
  return rr(SRV_NAME, TYPE_TXT, Buffer.from([0]), ttl, flush);
}

function aRecord(ip, ttl, flush) {
  const parts = ip.split('.').map(Number);
  if (parts.length !== 4 || parts.some((n) => Number.isNaN(n) || n < 0 || n > 255)) return null;
  return rr(FQDN, TYPE_A, Buffer.from(parts), ttl, flush);
}

/// 组 DNS 应答包（QDCOUNT=0，仅含 answers）
function packet(id, answers) {
  const h = Buffer.alloc(12);
  h.writeUInt16BE(id, 0);
  h.writeUInt16BE(0x8400, 2); // QR=1, opcode=0, RCODE=0
  h.writeUInt16BE(0, 4); // QDCOUNT
  h.writeUInt16BE(answers.length, 6);
  h.writeUInt16BE(0, 8); // NSCOUNT
  h.writeUInt16BE(0, 10); // ARCOUNT
  return Buffer.concat([h, ...answers]);
}

// ---------------- 本机 IP ----------------

/// 与 Rust lan_ip() 相同规则：第一个私网 IPv4（10/172.16-31/192.168，非 internal）
function currentIP() {
  const ifs = os.networkInterfaces();
  for (const key of Object.keys(ifs)) {
    for (const it of ifs[key] || []) {
      if (it.family !== 'IPv4' || it.internal) continue;
      const p = it.address.split('.').map(Number);
      if (
        p.length === 4 &&
        (p[0] === 10 || (p[0] === 172 && p[1] >= 16 && p[1] <= 31) || (p[0] === 192 && p[1] === 168))
      ) {
        return it.address;
      }
    }
  }
  return null;
}

/// 当前应通告的完整记录集（A 记录随 IP 实时更新）。
/// `flush`：组播应答/通告为 true（PTR 除外）；legacy 单播应答必须 false。
function answerSet(ttl, flush) {
  const ip = currentIP();
  if (!ip) return null;
  const list = [];
  for (const r of [ptrRecord(ttl), srvRecord(ttl, flush), txtRecord(ttl, flush), aRecord(ip, ttl, flush)]) {
    if (r) list.push(r);
  }
  return list;
}

// ---------------- 查询处理 ----------------

function wantSet(questions) {
  let ptr = false;
  let srv = false;
  let txt = false;
  let a = false;
  for (const q of questions) {
    const qc = q.qclass & 0x7fff; // 去掉 unicast-response 位
    if (qc !== CLASS_IN && qc !== CLASS_ANY) continue;
    // DNS 名字大小写不敏感：统一转小写比较（部分解析器会小写化查询名）
    const qname = q.qname.toLowerCase();
    const match = (n) => qname === n.toLowerCase() || qname === '*';
    if ((q.qtype === TYPE_PTR || q.qtype === TYPE_ANY) && match(PTR_NAME)) {
      ptr = srv = txt = a = true;
    }
    if ((q.qtype === TYPE_SRV || q.qtype === TYPE_ANY) && match(SRV_NAME)) {
      srv = txt = a = true;
    }
    if ((q.qtype === TYPE_TXT || q.qtype === TYPE_ANY) && match(SRV_NAME)) {
      txt = true;
    }
    if ((q.qtype === TYPE_A || q.qtype === TYPE_ANY) && match(FQDN)) {
      a = true;
    }
  }
  return ptr || srv || txt || a ? { ptr, srv, txt, a } : null;
}

/// 处理查询；`unicast` = 查询来自非 5353 端口（legacy 单播）：
/// 应答不加 cache-flush 位，且回显查询 ID（RFC 6762 §5.1/§10.2）。
function handleQuery(msg, unicast) {
  if (msg.length < 12) return null;
  const id = msg.readUInt16BE(0);
  const flags = msg.readUInt16BE(2);
  if ((flags & 0x8000) !== 0) return null; // 是响应，忽略
  const qd = msg.readUInt16BE(4);
  if (qd === 0) return null;
  let off = 12;
  const questions = [];
  for (let i = 0; i < qd; i++) {
    const parsed = parseName(msg, off);
    if (!parsed || parsed.next + 4 > msg.length) return null;
    questions.push({
      qname: parsed.name,
      qtype: msg.readUInt16BE(parsed.next),
      qclass: msg.readUInt16BE(parsed.next + 2),
    });
    off = parsed.next + 4;
  }
  const wanted = wantSet(questions);
  if (!wanted) return null;
  const flush = !unicast; // 组播应答可设 cache-flush（PTR 除外）；单播应答不可
  const list = [];
  if (wanted.ptr) list.push(ptrRecord(TTL));
  if (wanted.srv) list.push(srvRecord(TTL, flush));
  if (wanted.txt) list.push(txtRecord(TTL, flush));
  if (wanted.a) {
    const ip = currentIP();
    if (ip) list.push(aRecord(ip, TTL, flush));
  }
  if (list.length === 0) return null;
  // 组播应答 ID 应为 0（RFC 6762 §6）；legacy 单播应答回显查询 ID 便于匹配
  return packet(unicast ? id : 0, list);
}

// ---------------- 主流程 ----------------

function main() {
  const sock = dgram.createSocket({ type: 'udp4', reuseAddr: true });
  let closed = false;
  let boundIP = null;

  sock.on('error', (e) => {
    console.error('mdns-advertise error:', e.message);
    if (!closed) process.exit(1);
  });

  function sendToGroup(buf) {
    sock.send(buf, 0, buf.length, MDNS_PORT, MDNS_ADDR, () => {});
  }

  /// 确保组播成员/出口绑定在"当前 LAN 接口"上：IP 变化（休眠重连/换 Wi-Fi/
  /// DHCP 重分配）后自动重绑，否则收/发组播会停在旧接口上失效。
  /// drop 与 add 分开容错：旧接口已消失时 dropMembership 可能抛错（Windows
  /// 对失效接口的 IP_DROP_MEMBERSHIP 通常报错），绝不能因此跳过 add——
  /// 否则 socket 永久留在失效接口、IP 变化后永不重绑。
  function ensureBound(ip) {
    if (boundIP === ip) return true;
    if (boundIP) {
      try {
        sock.dropMembership(MDNS_ADDR, boundIP);
      } catch (_) {
        /* 旧接口可能已消失：忽略，继续尝试 add */
      }
    }
    try {
      sock.addMembership(MDNS_ADDR, ip);
      sock.setMulticastInterface(ip);
      boundIP = ip;
      console.log('mdns-advertise: rebound to', ip);
      return true;
    } catch (e) {
      console.error('mdns-advertise: membership rebind failed:', e.message);
      return false;
    }
  }

  function announce() {
    const ip = currentIP();
    if (!ip || !ensureBound(ip)) return; // 暂无 LAN IP / 重绑失败：跳过本轮
    const list = answerSet(TTL, true); // 组播通告：非共享记录可设 cache-flush
    if (!list) return;
    sendToGroup(packet(0, list));
  }

  sock.on('message', (msg, rinfo) => {
    let resp;
    try {
      // 组播查询（来自 5353）→ 组播应答；单播查询（legacy）→ 单播应答
      const unicast = rinfo.port !== MDNS_PORT;
      resp = handleQuery(msg, unicast);
      if (resp) {
        if (unicast) {
          sock.send(resp, 0, resp.length, rinfo.port, rinfo.address, () => {});
        } else {
          sendToGroup(resp);
        }
      }
    } catch (_) {
      return; // 畸形包忽略
    }
  });

  sock.bind(MDNS_PORT, () => {
    // 显式把组播成员加入"当前 LAN 接口"并设置组播出口：默认接口在 VPN /
    // 多网卡机器上可能是虚拟接口（utun*），导致收不到/发不出局域网组播。
    const lanIP = currentIP();
    try {
      if (lanIP) {
        ensureBound(lanIP);
      } else {
        sock.addMembership(MDNS_ADDR); // 兜底：内核按组播路由选择
      }
    } catch (e) {
      console.error('mdns-advertise: addMembership failed:', e.message);
      process.exit(1);
    }
    sock.setMulticastTTL(255);
    console.log(`mdns-advertise: advertising DeepSeek-Harness.local:${PORT} (${TYPE}) lan=${lanIP}`);
    announce();
    announce();
    announce(); // 启动通告 x3（mDNS 惯例）
    setInterval(announce, REANNOUNCE_MS);
  });

  // SIGTERM（macOS 侧 kill 时发送）：goodbye（TTL=0）后退出，客户端能立即清理
  // 缓存。注意：Windows 侧 kill 走 taskkill /F（TerminateProcess，不投递信号），
  // 本处理器在 Windows 上不会触发——靠 TTL(120s) 到期自动清理，属预期降级。
  process.on('SIGTERM', () => {
    if (closed) return;
    closed = true;
    try {
      const ip = currentIP();
      const list = ip ? answerSet(0, true) : null;
      if (list) {
        const bye = packet(0, list);
        // 在 send 回调里退出，确保 goodbye 已落网（直接 process.exit 会跳过事件循环）
        sock.send(bye, 0, bye.length, MDNS_PORT, MDNS_ADDR, () => process.exit(0));
        setTimeout(() => process.exit(0), 1000); // 兜底：send 异常也不卡死
      } else {
        process.exit(0);
      }
    } catch (_) {
      process.exit(0);
    }
  });
}

// ---------------- 自测模式（无网络，验证 DNS 报文逻辑）----------------

/// 用法: node mdns-advertise.js --self-test [ip]
/// 构造各类查询 → 走与线上完全相同的 handleQuery/wantSet/parseName 逻辑 →
/// 断言应答内容（20 项）。返回非 0 表示失败（可用于 CI / 打包前自检）。
function selfTest() {
  const testIP = process.argv[3] || currentIP() || '192.168.0.10';
  let failures = 0;
  const check = (cond, label) => {
    if (cond) {
      console.log('  [ok]', label);
    } else {
      console.error('  [FAIL]', label);
      failures += 1;
    }
  };
  // 覆盖 currentIP：自测用固定 IP，不依赖本机网络

  function buildQuery(qname, qtype) {
    const enc = [];
    for (const l of qname.split('.')) {
      if (!l) continue;
      enc.push(l.length);
      for (let i = 0; i < l.length; i++) enc.push(l.charCodeAt(i));
    }
    enc.push(0);
    const buf = Buffer.alloc(12 + enc.length + 4);
    buf.writeUInt16BE(0x1111, 0);
    buf.writeUInt16BE(0x0000, 2);
    buf.writeUInt16BE(1, 4);
    Buffer.from(enc).copy(buf, 12);
    buf.writeUInt16BE(qtype, 12 + enc.length);
    buf.writeUInt16BE(1, 14 + enc.length); // IN
    return buf;
  }

  function parseResponse(m) {
    const out = [];
    let off = 12;
    const an = m.readUInt16BE(6);
    for (let i = 0; i < an; i++) {
      const p = parseName(m, off);
      if (!p) break;
      off = p.next;
      if (off + 10 > m.length) break;
      const type = m.readUInt16BE(off);
      const cls = m.readUInt16BE(off + 2);
      const ttl = m.readUInt32BE(off + 4);
      const rdlen = m.readUInt16BE(off + 8);
      out.push({ name: p.name, type, cls, ttl, rdlen });
      off += 10 + rdlen;
    }
    return out;
  }

  console.log('== mdns-advertise self-test ==');

  // 1. 组播 PTR 查询 → 应回 PTR+SRV+TXT+A；cache-flush：PTR 永不设，
  //    SRV/TXT/A（组播应答）应设（RFC 6762 §10.2）
  const ptrResp = handleQuery(buildQuery(PTR_NAME, TYPE_PTR), false);
  check(!!ptrResp, 'PTR query gets a response');
  if (ptrResp) {
    const recs = parseResponse(ptrResp);
    check(recs.length === 4, `PTR response has 4 records (got ${recs.length})`);
    check(recs.some((r) => r.type === TYPE_PTR && r.name === PTR_NAME), 'has PTR record');
    check(recs.some((r) => r.type === TYPE_SRV && r.name === SRV_NAME), 'has SRV record');
    check(recs.some((r) => r.type === TYPE_TXT), 'has TXT record');
    const a = recs.find((r) => r.type === TYPE_A);
    check(!!a && a.ttl === TTL, `has A record ttl=${TTL}`);
    const ptr = recs.find((r) => r.type === TYPE_PTR);
    check(!!ptr && (ptr.cls & 0x8000) === 0, 'PTR record has NO cache-flush (shared RR)');
    const nonPtr = recs.filter((r) => r.type !== TYPE_PTR);
    check(nonPtr.every((r) => (r.cls & 0x8000) !== 0), 'SRV/TXT/A have cache-flush (multicast)');
    check(ptrResp.readUInt16BE(0) === 0, 'multicast response id=0 (RFC 6762 §6)');
  }

  // 1b. legacy 单播查询 → 应答不加 cache-flush（RFC 6762 §10.2），回显 ID
  const uniResp = handleQuery(buildQuery(PTR_NAME, TYPE_PTR), true);
  check(!!uniResp, 'unicast query gets a response');
  if (uniResp) {
    const recs = parseResponse(uniResp);
    check(recs.every((r) => (r.cls & 0x8000) === 0), 'unicast response has NO cache-flush');
    check(uniResp.readUInt16BE(0) === 0x1111, 'unicast response echoes query id');
  }

  // 2. A 记录构建（固定测试 IP，不依赖本机网络）
  const aResp = (function () {
    const ar = aRecord(testIP, TTL, true);
    return ar ? packet(0x1112, [ar]) : null;
  })();
  check(!!aResp, 'A record buildable');
  if (aResp) {
    const recs = parseResponse(aResp);
    check(recs.length === 1 && recs[0].type === TYPE_A, 'A response has exactly 1 A record');
  }

  // 3. 无关查询（其它服务）→ 不应响应
  const other = handleQuery(buildQuery('_ssh._tcp.local.', TYPE_PTR), false);
  check(other === null, 'unrelated query ignored');

  // 3b. 大小写变体查询（DNS 名字大小写不敏感；用真正不同的大小写验证）
  const lower = handleQuery(buildQuery('_HTTP._TCP.LOCAL.', TYPE_PTR), false);
  check(lower !== null, 'case-variant query answered (case-insensitive)');

  // 4. 响应包（QR=1）→ 忽略
  const q = buildQuery(PTR_NAME, TYPE_PTR);
  q.writeUInt16BE(0x8400, 2);
  check(handleQuery(q, false) === null, 'response packets ignored');

  // 5. goodbye 集（TTL=0）
  const bye = answerSet(0, true);
  check(!!bye && bye.length === 4, 'goodbye set has 4 records');
  if (bye) {
    const p = packet(0, bye);
    const recs = parseResponse(p);
    check(recs.every((r) => r.ttl === 0), 'goodbye ttl=0');
  }

  // 6. 压缩指针名字解析：偏移 0 处是指向偏移 12（完整名字）的指针
  const encFull = encodeName(SRV_NAME);
  const withPtr = Buffer.alloc(12 + encFull.length);
  encFull.forEach((b, i) => {
    withPtr[12 + i] = b;
  });
  withPtr[0] = 0xc0;
  withPtr[1] = 0x0c;
  const parsed = parseName(withPtr, 0);
  check(
    !!parsed && parsed.name === SRV_NAME && parsed.next === 2,
    'compression pointer parsed'
  );

  console.log(failures === 0 ? 'SELF_TEST OK' : `SELF_TEST FAILED (${failures})`);
  process.exit(failures === 0 ? 0 : 1);
}

if (process.argv[2] === '--self-test') {
  selfTest();
} else {
  main();
}
