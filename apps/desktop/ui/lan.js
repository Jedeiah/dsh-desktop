// 扫码远程连接：读取局域网状态 → 渲染二维码/地址/令牌；开关切换。
(() => {
  'use strict';
  const status = document.getElementById('status');
  const qrBox = document.getElementById('qrBox');
  const qrEl = document.getElementById('qr');
  const addrRow = document.getElementById('addrRow');
  const addrEl = document.getElementById('addr');
  const tokenRow = document.getElementById('tokenRow');
  const tokenEl = document.getElementById('token');
  const copyAddr = document.getElementById('copyAddr');
  const copyToken = document.getElementById('copyToken');
  const toggle = document.getElementById('toggle');
  const closeBtn = document.getElementById('closeBtn');

  // 无系统标题栏：✕ 与 ESC 关闭（普通弹窗）
  if (closeBtn) closeBtn.addEventListener('click', () => window.close());
  window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') window.close();
  });

  let busy = false;

  function setBusy(b) {
    busy = b;
    toggle.disabled = b;
  }

  function copyText(t) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(t).catch(() => {});
    }
  }

  function renderQr(url) {
    let ok = false;
    if (url && window.qrcode) {
      try {
        const qr = qrcode(0, 'M');
        qr.addData(url);
        qr.make();
        qrEl.innerHTML = qr.createImgTag(6, 8);
        ok = true;
      } catch (e) { /* fallthrough */ }
    }
    if (!ok) qrEl.innerHTML = '';
    return ok;
  }

  async function refresh() {
    setBusy(true);
    try {
      const st = await window.__TAURI__.core.invoke('lan_state');
      const on = !!st.enabled;
      status.textContent = on ? '已开启' : '未开启';
      status.className = 'status-line ' + (on ? 'on' : 'off');
      toggle.textContent = on ? '关闭手机远程连接' : '开启手机远程连接';
      toggle.classList.toggle('off', !on);
      // 二维码与信息只在开启时展示
      qrBox.hidden = !on;
      addrRow.hidden = !on;
      tokenRow.hidden = !on;
      copyAddr.hidden = !on;
      copyToken.hidden = !on;
      if (on) {
        renderQr(st.qr_url || '');
        addrEl.textContent = 'http://' + st.ip + ':' + st.port;
        tokenEl.textContent = st.token || '';
        qrEl.setAttribute('data-url', st.qr_url || '');
      }
    } catch (e) {
      status.textContent = '读取状态失败：' + (e.message || e);
      status.className = 'status-line err';
    } finally {
      setBusy(false);
    }
  }

  toggle.addEventListener('click', async () => {
    if (busy) return;
    const enable = toggle.textContent.indexOf('开启') === 0;
    setBusy(true);
    try {
      status.textContent = enable ? '正在开启…' : '正在关闭…';
      status.className = 'status-line';
      await window.__TAURI__.core.invoke('lan_toggle', { enable });
      await refresh();
    } catch (e) {
      status.textContent = '操作失败：' + (e.message || e);
      status.className = 'status-line err';
      setBusy(false);
    }
  });

  copyAddr.addEventListener('click', () => copyText(addrEl.textContent));
  copyToken.addEventListener('click', () => copyText(tokenEl.textContent));

  refresh();
})();
