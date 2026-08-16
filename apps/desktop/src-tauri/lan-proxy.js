#!/usr/bin/env node
// DSh Desktop — 局域网转发器（带令牌鉴权）
//
// 用途：让同一局域网内的手机/其他设备通过浏览器访问 dsh web。
// dsh 出于安全只绑定 127.0.0.1；本转发器监听所有网卡，把经过令牌
// 验证的请求（含 WebSocket）转发给本机 dsh，从 dsh 视角看所有连接
// 都来自 127.0.0.1，因此无需 --trusted-host，安全模型不变。
//
// 用法: node lan-proxy.js <dshPort> <token> <listenPort>
//   - 未登录请求 -> 302 到 /__lan_login 登录页
//   - 登录成功 -> Set-Cookie (30 天) -> 302 回 /
//   - 带有效 cookie 的 HTTP/WS -> 转发给 127.0.0.1:<dshPort>
const http = require('http');
const crypto = require('crypto');

const DSH_PORT = process.argv[2];
const TOKEN = process.argv[3];
const LISTEN = Number(process.argv[4] || 3190);
const TARGET = `http://127.0.0.1:${DSH_PORT}`;
const COOKIE_NAME = 'dsh_lan_token';
const TTL_SECONDS = 30 * 24 * 3600; // 30 天
const LOGIN_PATH = '/__lan_login';

function safeEqual(a, b) {
  const ba = Buffer.from(String(a));
  const bb = Buffer.from(String(b));
  if (ba.length !== bb.length) return false;
  return crypto.timingSafeEqual(ba, bb);
}

// 登录页：与壳应用启动页同一设计语言（粉紫蓝漂移光斑 + 动态噪点 + 深紫玻璃卡片）。
function loginPage(err) {
  return `<!doctype html>
<html lang="zh-CN">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width,initial-scale=1" />
    <title>DeepSeek Harness 访问验证</title>
    <style>
      * { margin: 0; padding: 0; box-sizing: border-box; }

      body {
        font-family: -apple-system, system-ui, 'PingFang SC', 'Microsoft YaHei', sans-serif;
        background: #05060a;
        color: #f0f2f7;
        display: flex;
        align-items: center;
        justify-content: center;
        height: 100vh;
        margin: 0;
        overflow: hidden;
      }

      /* ===== 高饱和氛围光斑 + 漂移动画（与启动页一致） ===== */
      .glow {
        position: fixed;
        z-index: 0;
        border-radius: 50%;
        filter: blur(100px);
        pointer-events: none;
        will-change: transform;
        animation: drift 20s ease-in-out infinite alternate;
      }
      .glow--a {
        width: 46vw; height: 46vw; left: -12vw; top: -8vh;
        background: radial-gradient(circle, rgba(250, 147, 250, 0.55) 0%, rgba(250, 147, 250, 0) 70%);
        animation-duration: 18s;
      }
      .glow--b {
        width: 42vw; height: 42vw; right: -10vw; top: 24vh;
        background: radial-gradient(circle, rgba(152, 58, 214, 0.55) 0%, rgba(152, 58, 214, 0) 70%);
        animation-duration: 24s;
        animation-delay: -6s;
      }
      .glow--c {
        width: 50vw; height: 50vw; left: 22vw; bottom: -22vh;
        background: radial-gradient(circle, rgba(47, 129, 247, 0.5) 0%, rgba(47, 129, 247, 0) 70%);
        animation-duration: 28s;
        animation-delay: -12s;
      }
      @keyframes drift {
        from { transform: translate3d(0, 0, 0) scale(1); }
        to   { transform: translate3d(6vw, -4vh, 0) scale(1.15); }
      }

      /* ===== 动态噪点（与启动页一致） ===== */
      body::after {
        content: '';
        position: fixed;
        inset: 0;
        z-index: 0;
        background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='160' height='160'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='0.8' numOctaves='3' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='100%25' height='100%25' filter='url(%23n)'/%3E%3C/svg%3E");
        opacity: 0.06;
        mix-blend-mode: overlay;
        pointer-events: none;
        animation: grainShift 0.5s steps(4) infinite;
      }
      @keyframes grainShift {
        0%   { background-position: 0 0; }
        25%  { background-position: -20px 10px; }
        50%  { background-position: 10px -20px; }
        75%  { background-position: -10px 20px; }
        100% { background-position: 0 0; }
      }

      /* ===== 深紫玻璃卡片（启动页玻璃胶囊的放大版） ===== */
      .card {
        position: relative;
        z-index: 1;
        width: min(340px, 90vw);
        padding: 36px 30px 28px;
        border-radius: 20px;
        background: rgba(28, 27, 36, 0.45);
        backdrop-filter: blur(20px);
        -webkit-backdrop-filter: blur(20px);
        box-shadow:
          0 0 34px rgba(201, 103, 232, 0.22),
          inset 0 1px 1px rgba(255, 255, 255, 0.15),
          0 20px 60px rgba(0, 0, 0, 0.45);
      }
      .card::before {
        content: '';
        position: absolute;
        inset: 0;
        border-radius: inherit;
        padding: 1.5px;
        background: linear-gradient(180deg,
          rgba(255,255,255,0.55) 0%,
          rgba(255,255,255,0.15) 40%,
          rgba(255,255,255,0.15) 60%,
          rgba(255,255,255,0.55) 100%);
        -webkit-mask: linear-gradient(#fff 0 0) content-box, linear-gradient(#fff 0 0);
        -webkit-mask-composite: xor;
                mask: linear-gradient(#fff 0 0) content-box, linear-gradient(#fff 0 0);
                mask-composite: exclude;
        pointer-events: none;
      }

      h1 {
        font-size: 17px;
        font-weight: 600;
        letter-spacing: 0.01em;
        margin: 0 0 4px;
        color: #f0f2f7;
      }
      .sub {
        font-size: 13px;
        color: rgba(240, 242, 247, 0.55);
        margin-bottom: 20px;
      }

      /* 玻璃输入框（聚焦紫色光环，配套启动页的紫色系） */
      input {
        width: 100%;
        box-sizing: border-box;
        padding: 10px 12px;
        border-radius: 10px;
        border: 1px solid rgba(255, 255, 255, 0.14);
        background: rgba(255, 255, 255, 0.06);
        backdrop-filter: blur(10px);
        -webkit-backdrop-filter: blur(10px);
        color: #f0f2f7;
        font-size: 15px;
        margin: 0 0 14px;
        outline: none;
        transition: border-color 0.2s ease, box-shadow 0.2s ease;
      }
      input::placeholder { color: rgba(240, 242, 247, 0.35); }
      input:focus {
        border-color: rgba(201, 103, 232, 0.65);
        box-shadow: 0 0 0 3px rgba(152, 58, 214, 0.2), 0 0 16px rgba(201, 103, 232, 0.25);
      }

      /* 玻璃态按钮（半透明紫色底 + blur + 渐变描边 + 紫色光晕，短款居中） */
      button {
        position: relative;
        display: block;
        width: 160px;
        margin: 0 auto;
        padding: 10px 12px;
        border-radius: 10px;
        border: none;
        background: linear-gradient(180deg, rgba(201, 103, 232, 0.16), rgba(122, 90, 248, 0.10));
        backdrop-filter: blur(16px);
        -webkit-backdrop-filter: blur(16px);
        color: #f0f2f7;
        font-size: 15px;
        font-weight: 500;
        cursor: pointer;
        box-shadow:
          inset 0 1px 1px rgba(255, 255, 255, 0.15),
          0 0 18px rgba(201, 103, 232, 0.18);
        transition: background 0.2s ease, box-shadow 0.2s ease, transform 0.15s ease;
      }
      /* 渐变描边（与卡片同款） */
      button::before {
        content: '';
        position: absolute;
        inset: 0;
        border-radius: inherit;
        padding: 1.3px;
        background: linear-gradient(180deg,
          rgba(255,255,255,0.5) 0%,
          rgba(255,255,255,0.14) 40%,
          rgba(255,255,255,0.14) 60%,
          rgba(255,255,255,0.5) 100%);
        -webkit-mask: linear-gradient(#fff 0 0) content-box, linear-gradient(#fff 0 0);
        -webkit-mask-composite: xor;
                mask: linear-gradient(#fff 0 0) content-box, linear-gradient(#fff 0 0);
                mask-composite: exclude;
        pointer-events: none;
      }
      button:hover {
        background: linear-gradient(180deg, rgba(201, 103, 232, 0.26), rgba(122, 90, 248, 0.16));
        box-shadow:
          inset 0 1px 1px rgba(255, 255, 255, 0.2),
          0 0 28px rgba(201, 103, 232, 0.4);
      }
      button:active { transform: scale(0.98); }

      .err {
        color: #f85149;
        font-size: 13px;
        margin-top: 12px;
        text-align: center;
        text-shadow: 0 0 12px rgba(248, 81, 73, 0.4);
      }

      /* 系统开启"减少动态效果"时停用全部动画 */
      @media (prefers-reduced-motion: reduce) {
        .glow, body::after { animation: none; }
      }
    </style>
  </head>
  <body>
    <div class="glow glow--a" aria-hidden="true"></div>
    <div class="glow glow--b" aria-hidden="true"></div>
    <div class="glow glow--c" aria-hidden="true"></div>

    <form class="card" method="post" action="${LOGIN_PATH}">
      <h1>DeepSeek Harness</h1>
      <div class="sub">请输入访问令牌继续</div>
      <input name="token" type="password" placeholder="访问令牌" autofocus required />
      <button type="submit">进入</button>
      ${err ? `<div class="err" role="alert">${err}</div>` : ''}
    </form>
  </body>
</html>`;
}

function hasValidCookie(req) {
  const c = req.headers.cookie || '';
  for (const part of c.split(';')) {
    const i = part.indexOf('=');
    if (i < 0) continue;
    const k = part.slice(0, i).trim();
    const v = part.slice(i + 1).trim();
    if (k === COOKIE_NAME && safeEqual(v, TOKEN)) return true;
  }
  return false;
}

// ---------------------------------------------------------------------------
// 连接鲁棒性：任何一端异常断开（浏览器取消请求、关标签页、Wi-Fi 抖动、WS
// 中断）都会让 socket 触发 'error'。node 里不监听 'error' 的 socket 一旦报错
// 会直接抛 unhandled 'error' 事件把整个进程打崩 —— 这正是之前"手机用一会儿
// 就全 load failed"的根因（代理挂了，但界面是缓存的 SPA，路由还在，只有数据
// 请求全部失败）。因此给每一类 socket 都挂上 error 兜底，错误只关掉对应连接。
// ---------------------------------------------------------------------------
function silence(socket) {
  if (socket && typeof socket.on === 'function') socket.on('error', () => {});
  return socket;
}

const server = http.createServer((req, res) => {
  silence(req);
  silence(res);
  const path = (req.url || '/').split('?')[0];

  if (path === LOGIN_PATH) {
    if (req.method === 'POST') {
      let body = '';
      req.on('data', (d) => (body += d));
      req.on('end', () => {
        const tok = new URLSearchParams(body).get('token') || '';
        if (safeEqual(tok, TOKEN)) {
          res.writeHead(302, {
            'Set-Cookie': `${COOKIE_NAME}=${TOKEN}; Path=/; Max-Age=${TTL_SECONDS}; HttpOnly; SameSite=Strict`,
            Location: '/',
          });
          res.end();
        } else {
          res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
          res.end(loginPage('令牌不正确，请重试'));
        }
      });
      return;
    }
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(loginPage(''));
    return;
  }

  if (!hasValidCookie(req)) {
    res.writeHead(302, { Location: LOGIN_PATH });
    res.end();
    return;
  }

  forward(req, res);
});

// 未升级的客户端连接兜底（含握手前就断开的连接）。
server.on('connection', (socket) => silence(socket));
// 畸形请求（坏请求行/坏头）默认回 400，而不是让进程崩溃。
server.on('clientError', (err, socket) => {
  silence(socket);
  if (socket.writable) {
    socket.end('HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n');
  } else {
    socket.destroy();
  }
});

// WebSocket 透传（dsh 的 agent 流走 WS，需同样鉴权）
server.on('upgrade', (req, socket, head) => {
  silence(req);
  silence(socket);
  if (!hasValidCookie(req)) {
    socket.destroy();
    return;
  }
  const preq = http.request(TARGET + req.url, {
    method: req.method,
    headers: cleanHeaders(req.headers),
  });
  preq.on('upgrade', (pres, psocket, phead) => {
    silence(psocket); // 目标侧握手后异常断开：只关连接，不崩进程
    socket.write(`HTTP/1.1 101 ${pres.statusMessage || 'Switching Protocols'}\r\n`);
    for (const [k, v] of Object.entries(pres.headers)) socket.write(`${k}: ${v}\r\n`);
    socket.write('\r\n');
    socket.write(phead); // 目标缓冲 -> 客户端
    psocket.write(head); // 客户端缓冲 -> 目标
    socket.pipe(psocket);
    psocket.pipe(socket);
  });
  preq.on('response', () => socket.destroy());
  preq.on('error', () => socket.destroy());
  preq.end();
});

// ---------------------------------------------------------------------------
// 转发头清洗：dsh 的 /api 篱笆要求 "Host 为 loopback 且 Origin 与 Host 一致"。
// 手机页面 Origin 是 http://<LAN-IP>:端口，与改写成 127.0.0.1 的 Host 不匹配
// → 会 403。因此转发时剥离 origin/sec-fetch-site/referer（dsh 视为本机/同源）。
// ---------------------------------------------------------------------------
function cleanHeaders(headers) {
  const h = { ...headers };
  delete h['origin'];
  delete h['sec-fetch-site'];
  delete h['referer'];
  h['host'] = new URL(TARGET).host; // loopback，过篱笆
  return h;
}

// ---------------------------------------------------------------------------
// 安全上下文 polyfill：手机用明文 http://<LAN-IP> 访问，页面属非安全上下文，
// crypto.randomUUID 不可用（dsh 前端用它初始化会话，缺失→会话/项目渲染为空）。
// 注入到主文档 <head> 前即可；只处理 text/html 主文档。
// ---------------------------------------------------------------------------
const RANDOM_UUID_POLYFILL = `<script>if(!crypto.randomUUID){crypto.randomUUID=function(){var b=crypto.getRandomValues(new Uint8Array(16));b[6]=(b[6]&0x0f)|0x40;b[8]=(b[8]&0x3f)|0x80;var h=Array.prototype.map.call(b,function(x){return ('0'+x.toString(16)).slice(-2)});return h.slice(0,4).join('')+'-'+h.slice(4,6).join('')+'-'+h.slice(6,8).join('')+'-'+h.slice(8,10).join('')+'-'+h.slice(10).join('')}};</script>`;

// ---------------------------------------------------------------------------
// 远程设置持久化补丁：dsh 前端把"设置是否可写"与页面 hostname 是否 loopback
// 绑定（connection.isLoopback ? "host" : "memory"）。经本代理访问时 hostname
// 是 LAN IP（如 192.168.1.2），判定为非 loopback → 设置作用域降级为 memory：
// settings.mutate 根本不会发出（主题、内测声明确认等改动只存活于当前页面内存，
// 刷新即丢）。这里把 dsh-client-connection bundle 里的 isLoopbackHostname
// 恒真化，让远程页面也走 host 持久化：写操作经代理转发（代理转发时 Host 已改
// 回 loopback，服务端篱笆放行）落盘到本机 ~/.dsh/settings.yaml，手机与桌面端
// 设置双向同步。访问本身仍受令牌鉴权约束——等价于"能通过令牌访问的人"视为
// 本机用户。bundle 内容按 ?rev= 缓存；上游 bundle 变化导致模式不匹配时直通
// 原内容（保持 memory 旧行为），并输出警告便于排查。
// ---------------------------------------------------------------------------
const CONNECTION_BUNDLE_RE = /\/plugins\/@deepseek-ai\/dsh-client-connection\/client\.js(?:\?|$)/;
const LOOPBACK_PATCH_RE = /function isLoopbackHostname\(hostname\)\s*\{[\s\S]*?\n\s*\}/;
const LOOPBACK_PATCH_TO = 'function isLoopbackHostname(hostname) { return true; }';
const PATCH_CACHE = new Map(); // req.url -> { original, body }
const PATCH_CACHE_MAX = 16;

function forward(req, res) {
  const preq = http.request(TARGET + req.url, {
    method: req.method,
    headers: cleanHeaders(req.headers),
  }, (pres) => {
    const ct = pres.headers['content-type'] || '';
    const enc = pres.headers['content-encoding']; // gzip 时不能改写（会破坏压缩流）
    const isHtml = ct.includes('text/html') && req.method === 'GET'; // HEAD 无 body，直通即可
    const isConnectionBundle =
      ct.includes('javascript') && CONNECTION_BUNDLE_RE.test(req.url) && req.method === 'GET';
    pres.on('error', () => { // 上游（dsh）在响应中途断开
      if (!res.headersSent) { res.writeHead(502); res.end(); } else { res.destroy(); }
    });
    if (isConnectionBundle && !enc) {
      // 缓冲 connection bundle 并打 isLoopbackHostname 补丁（content-length 会变化，需删除）
      const chunks = [];
      pres.on('data', (d) => chunks.push(d));
      pres.on('end', () => {
        const original = Buffer.concat(chunks).toString('utf8');
        let cached = PATCH_CACHE.get(req.url);
        if (!cached || cached.original !== original) {
          const patched = original.replace(LOOPBACK_PATCH_RE, LOOPBACK_PATCH_TO);
          // 断言补丁后必须是"签名+return true+闭合"的完整形态；若上游重构出独立成行的
          // 嵌套 }，正则可能提前截断产出损坏 JS，此时视为未命中、直通原内容。
          const ok = patched !== original &&
            /function isLoopbackHostname\(hostname\)\s*\{\s*return true;\s*\}/.test(patched);
          cached = { original, body: ok ? patched : original };
          if (PATCH_CACHE.size >= PATCH_CACHE_MAX) PATCH_CACHE.delete(PATCH_CACHE.keys().next().value);
          PATCH_CACHE.set(req.url, cached);
          if (!ok) {
            console.warn('[lan-proxy] isLoopbackHostname 补丁未命中（上游 bundle 已变化？），直通原内容');
          }
        }
        const headers = { ...pres.headers };
        delete headers['content-length'];
        res.writeHead(pres.statusCode, headers);
        res.end(cached.body);
      });
      return;
    }
    if (isHtml && !enc) {
      // 缓冲主文档并注入 polyfill（content-length 会变化，需删除）
      const chunks = [];
      pres.on('data', (d) => chunks.push(d));
      pres.on('end', () => {
        let html = Buffer.concat(chunks).toString('utf8');
        if (/<\/head>/i.test(html)) {
          html = html.replace(/<\/head>/i, RANDOM_UUID_POLYFILL + '</head>');
        } else {
          html = RANDOM_UUID_POLYFILL + html;
        }
        const headers = { ...pres.headers };
        delete headers['content-length'];
        res.writeHead(pres.statusCode, headers);
        res.end(html);
      });
      return;
    }
    res.writeHead(pres.statusCode, pres.headers);
    pres.pipe(res);
  });
  req.pipe(preq);
  req.on('error', () => {}); // 客户端在上传中途断开
  res.on('error', () => {}); // 客户端在响应中途断开（最常见：关标签页/刷新）
  preq.on('error', () => { if (!res.headersSent) { res.writeHead(502); res.end(); } else res.destroy(); });
}

server.listen(LISTEN, '0.0.0.0', () => {
  console.log(`[lan-proxy] listening :${LISTEN} -> ${TARGET}`);
});

// 启动自检：确认补丁模式与当前上游 connection bundle 匹配，避免"静默失效"。
(function selfCheck() {
  const probe = http.get(`${TARGET}/plugins/@deepseek-ai/dsh-client-connection/client.js`, (r) => {
    const chunks = [];
    r.on('data', (d) => chunks.push(d));
    r.on('end', () => {
      const js = Buffer.concat(chunks).toString('utf8');
      console.log(LOOPBACK_PATCH_RE.test(js)
        ? '[lan-proxy] isLoopbackHostname 补丁模式匹配 ✓ 远程设置将走 host 持久化'
        : '[lan-proxy] ⚠ isLoopbackHostname 补丁模式未匹配（上游 bundle 已变化？），远程设置保持 memory 模式');
    });
  });
  probe.on('error', () => {
    console.warn('[lan-proxy] 补丁自检失败（上游未响应？），跳过自检');
  }); // 自检失败不阻塞服务
})();
