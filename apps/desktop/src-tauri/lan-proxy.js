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

function loginPage(err) {
  return `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>DeepSeek Harness 访问验证</title>
<style>
body{font-family:-apple-system,system-ui,sans-serif;background:#0d1117;color:#e6edf3;
display:flex;align-items:center;justify-content:center;height:100vh;margin:0}
.card{background:#161b22;border:1px solid #30363d;border-radius:12px;padding:32px;width:min(360px,90vw)}
h1{font-size:18px;margin:0 0 4px}
.sub{font-size:13px;color:#8b949e;margin-bottom:8px}
input{width:100%;box-sizing:border-box;padding:10px;border-radius:8px;border:1px solid #30363d;
background:#0d1117;color:#e6edf3;font-size:16px;margin:12px 0}
button{width:100%;padding:10px;border-radius:8px;border:none;background:#2f81f7;color:#fff;font-size:16px;cursor:pointer}
.err{color:#f85149;font-size:13px;margin-top:8px}
</style></head><body>
<form class="card" method="post" action="${LOGIN_PATH}">
<h1>DeepSeek Harness</h1>
<div class="sub">请输入访问令牌继续</div>
<input name="token" type="password" placeholder="访问令牌" autofocus required>
<button type="submit">进入</button>
${err ? `<div class="err">${err}</div>` : ''}
</form></body></html>`;
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
// 注入到主文档 <head> 前即可；只处理 text/html 主文档，插件 bundle 走直通。
// ---------------------------------------------------------------------------
const RANDOM_UUID_POLYFILL = `<script>if(!crypto.randomUUID){crypto.randomUUID=function(){var b=crypto.getRandomValues(new Uint8Array(16));b[6]=(b[6]&0x0f)|0x40;b[8]=(b[8]&0x3f)|0x80;var h=Array.prototype.map.call(b,function(x){return ('0'+x.toString(16)).slice(-2)});return h.slice(0,4).join('')+'-'+h.slice(4,6).join('')+'-'+h.slice(6,8).join('')+'-'+h.slice(8,10).join('')+'-'+h.slice(10).join('')}};</script>`;

function forward(req, res) {
  const preq = http.request(TARGET + req.url, {
    method: req.method,
    headers: cleanHeaders(req.headers),
  }, (pres) => {
    const ct = pres.headers['content-type'] || '';
    const enc = pres.headers['content-encoding']; // gzip 时不能注入（会破坏压缩流）
    const isHtml = ct.includes('text/html') && (req.method === 'GET' || req.method === 'HEAD');
    pres.on('error', () => { // 上游（dsh）在响应中途断开
      if (!res.headersSent) { res.writeHead(502); res.end(); } else { res.destroy(); }
    });
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
