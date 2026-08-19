// 壳页 shell.js：工作台(iframe) + 管理 Tabs（常规/网络/插件/更新/卸载）
// 所有管理能力内嵌于主窗（window.__TAURI__ 只注入主 frame —— 远程 iframe 拿不到
// IPC，安全面收窄）。⌘K 打开命令面板。Esc 在管理页返回工作台。
(() => {
  'use strict';
  const $ = (id) => document.getElementById(id);
  const T = window.__TAURI__;
  const invoke = (...a) => T.core.invoke(...a);

  // ---------------- Tab 切换 ----------------
  const tabs = $('tabs').querySelectorAll('.tab');
  const tabOf = (name) => {
    if (name === 'workbench') {
      document.querySelectorAll('.panel').forEach((p) => p.classList.remove('active'));
      $('activity-workbench').style.display = 'block';
      return;
    }
    ['general', 'lan', 'plugins', 'update', 'uninstall'].forEach((n) => {
      const active = n === name;
      $('panel-' + n).classList.toggle('active', active);
    });
    $('activity-workbench').style.display = 'none';
  };
  function selectTab(name) {
    tabs.forEach((t) => t.classList.toggle('active', t.dataset.tab === name));
    tabOf(name);
  }
  tabs.forEach((t) => t.addEventListener('click', () => selectTab(t.dataset.tab)));

  // ⌘K / Ctrl+K 命令面板
  const paletteNames = {
    workbench: '工作台', general: '常规', lan: '网络',
    plugins: '插件', update: '更新', uninstall: '卸载',
  };
  window.addEventListener('keydown', (e) => {
    if ((e.metaKey || e.ctrlKey) && (e.key === 'k' || e.key === 'K')) {
      e.preventDefault();
      const names = ['workbench', 'general', 'lan', 'plugins', 'update', 'uninstall'];
      const label = '⌘K 跳转：' + names.map((n) => paletteNames[n]).join(' / ');
      // 用一个轻量原生提示即可（真实命令面板见二期；此处给键盘可达性）
      const cur = [...tabs].find((t) => t.classList.contains('active'));
      const idx = cur ? names.indexOf(cur.dataset.tab) : 0;
      const next = names[(idx + 1) % names.length];
      selectTab(next);
      const statusEl = document.querySelector('.panel.active .status');
      if (statusEl) { statusEl.textContent = label; statusEl.className = 'status'; setTimeout(() => (statusEl.textContent = ''), 1600); }
    }
    // Esc：管理页返回工作台
    if (e.key === 'Escape') {
      const activePanel = document.querySelector('.panel.active');
      if (activePanel) selectTab('workbench');
    }
  });

  // ---------------- 工作台 iframe ----------------
  const wb = $('workbenchFrame');
  const wbPlaceholder = $('wbPlaceholder');
  let lastUrl = '';

  function showPlaceholder() {
    // 先清内联 display 再取消 hidden（hidden=false 但残留 inline display:none
    // 时仍会隐藏；反之先 hidden=false 再清 display 会出现一帧闪变）
    wbPlaceholder.style.display = '';
    wbPlaceholder.hidden = false;
  }
  function hidePlaceholder() {
    // hidden 属性 + 显式 display 双保险（.placeholder 的 display:flex 会覆盖
    // hidden 的默认 display:none；shell.html 已补 [hidden]{display:none!important}）
    wbPlaceholder.hidden = true;
    wbPlaceholder.style.display = 'none';
  }

  // 占位层是绝对定位覆盖在 iframe 之上：在 iframe 真正绘制完成（onload）前
  // 一直盖住，把"白屏/空白 iframe"阶段用 spinner 遮住，直到 onload 才撤。
  // 这修复了"loding 没了 → 白屏 → 又 loding"的闪烁根因：不再 src 一赋值就
  // 立刻撤占位（此时 iframe 尚未来得及绘制，露出白底）。
  wb.addEventListener('load', () => hidePlaceholder());
  wb.addEventListener('error', () => showPlaceholder());

  function loadWorkbench(url) {
    if (!url || url === lastUrl) return;
    lastUrl = url;
    showPlaceholder(); // 换端口/重载路径：先盖住，onload 后撤
    wb.src = url;
    wb.focus();
  }

  // 加载 dsh 工作台：以「轮询 get_dsh_url」为主通道（普通 invoke，必通），
  // 事件监听仅作增量/次选（且必须带超时，避免 T.event.listen 在此路径挂起而
  // 把后续逻辑全部堵死——此前实测该调用在本页面会挂起）。
  (async () => {
    // 1) 先轮询：每 800ms 拉一次，最多 15 次（约 12s），dsh 就绪即有值。
    for (let i = 0; i < 15; i++) {
      try {
        const url = await invoke('get_dsh_url');
        if (url) { loadWorkbench(url); break; }
      } catch (e) { break; }
      await new Promise(r => setTimeout(r, 800));
    }
    // 2) 事件监听（增量/换端口用）：3s 超时，不再阻塞主流程。
    try {
      await Promise.race([
        T.event.listen('dsh:url', (ev) => loadWorkbench(ev.payload)),
        new Promise((_, rej) => setTimeout(() => rej(new Error('listen-timeout')), 3000)),
      ]);
    } catch (e) { /* 能力缺失/超时：轮询已兜底 */ }
  })();

  // ---------------- 常规 ----------------
  const cwdEl = $('cwd');
  const loginBtn = $('btnLogin');

  function setLoginLabel(on) {
    loginBtn.textContent = on ? '关闭登录自启' : '开启登录自启';
  }

  (async () => {
    try {
      const st = await invoke('get_shell_state');
      cwdEl.textContent = st.cwd || '（默认）';
      setLoginLabel(!!st.login_on);
    } catch (e) { /* ignore */ }
  })();

  $('btnChooseWs').addEventListener('click', async () => {
    try {
      const st = await invoke('choose_workspace_cmd');
      cwdEl.textContent = st || '（默认）';
    } catch (e) { /* user cancelled */ }
  });
  $('btnOpenWs').addEventListener('click', () => invoke('open_workspace_cmd').catch(() => {}));
  $('btnRestart').addEventListener('click', () => invoke('restart_dsh_cmd').catch(() => {}));
  $('btnOpenLogs').addEventListener('click', () => invoke('open_logs_cmd').catch(() => {}));
  $('btnBrowser').addEventListener('click', () => invoke('open_browser_cmd').catch(() => {}));

  loginBtn.addEventListener('click', async () => {
    const next = loginBtn.textContent.indexOf('关闭') === 0 ? false : true;
    try {
      await invoke('set_login_cmd', { enable: next });
      setLoginLabel(next);
    } catch (e) {
      showNotify('登录自启设置失败', e.message || e);
    }
  });

  // ---------------- 网络（扫码远程连接） ----------------
  const lanStatus = $('lanStatus');
  const lanQrBox = $('lanQrBox');
  const lanQrEl = $('lanQr');
  const lanAddrRow = $('lanAddrRow');
  const lanAddrEl = $('lanAddr');
  const lanTokenRow = $('lanTokenRow');
  const lanTokenEl = $('lanToken');
  const lanCopyAddr = $('lanCopyAddr');
  const lanCopyToken = $('lanCopyToken');
  const btnLanToggle = $('btnLanToggle');
  let lanBusy = false;

  function copyText(t) {
    if (navigator.clipboard && navigator.clipboard.writeText) navigator.clipboard.writeText(t).catch(() => {});
  }
  function renderQr(url) {
    let ok = false;
    if (url && window.qrcode) {
      try {
        const qr = qrcode(0, 'M');
        qr.addData(url);
        qr.make();
        lanQrEl.innerHTML = qr.createImgTag(6, 8);
        ok = true;
      } catch (e) { /* fallthrough */ }
    }
    if (!ok) lanQrEl.innerHTML = '';
    return ok;
  }
  async function refreshLan() {
    lanBusy = true;
    btnLanToggle.disabled = true;
    try {
      const st = await invoke('lan_state');
      const on = !!st.enabled;
      lanStatus.textContent = on ? '已开启' : '未开启';
      lanStatus.className = 'status ' + (on ? 'ok' : '');
      btnLanToggle.textContent = on ? '关闭手机远程连接' : '开启手机远程连接';
      btnLanToggle.classList.toggle('off', !on);
      lanQrBox.hidden = !on;
      lanAddrRow.hidden = !on;
      lanTokenRow.hidden = !on;
      lanCopyAddr.hidden = !on;
      lanCopyToken.hidden = !on;
      if (on) {
        renderQr(st.qr_url || '');
        lanAddrEl.textContent = 'http://' + st.ip + ':' + st.port;
        lanTokenEl.textContent = st.token || '';
      }
    } catch (e) {
      lanStatus.textContent = '读取状态失败：' + (e.message || e);
      lanStatus.className = 'status err';
    } finally {
      lanBusy = false;
      btnLanToggle.disabled = false;
    }
  }
  btnLanToggle.addEventListener('click', async () => {
    if (lanBusy) return;
    const enable = btnLanToggle.textContent.indexOf('开启') === 0;
    try {
      lanStatus.textContent = enable ? '正在开启…' : '正在关闭…';
      lanStatus.className = 'status';
      await invoke('lan_toggle', { enable });
      await refreshLan();
    } catch (e) {
      lanStatus.textContent = '操作失败：' + (e.message || e);
      lanStatus.className = 'status err';
    }
  });
  lanCopyAddr.addEventListener('click', () => copyText(lanAddrEl.textContent));
  lanCopyToken.addEventListener('click', () => copyText(lanTokenEl.textContent));

  // ---------------- 插件 ----------------
  const pkgEl = $('pkg');
  const btnInstall = $('btnInstall');
  const btnRemove = $('btnRemove');
  const pluginStatus = $('pluginStatus');
  const pluginOutput = $('pluginOutput');
  let unlistenPlugin = null;

  function setPluginStatus(text, kind) {
    pluginStatus.textContent = text;
    pluginStatus.className = 'status ' + (kind || '');
  }
  function setPluginBusy(busy) {
    btnInstall.disabled = busy;
    btnRemove.disabled = busy;
    pkgEl.disabled = busy;
  }
  try {
    T.event.listen('dsh:plugin-output', (e) => {
      pluginOutput.hidden = false;
      pluginOutput.textContent += e.payload + '\n';
      pluginOutput.scrollTop = pluginOutput.scrollHeight;
    }).then((fn) => { unlistenPlugin = fn; }).catch(() => {});
  } catch (e) { /* 降级 */ }

  async function runPlugin(op) {
    const name = pkgEl.value.trim();
    if (!name) { setPluginStatus('请输入包名', 'err'); return; }
    setPluginBusy(true);
    setPluginStatus(op === 'add' ? '正在安装…（首次需下载依赖，可能较久）' : '正在卸载…');
    pluginOutput.hidden = false;
    pluginOutput.textContent = '';
    try {
      const text = await invoke('plugin_op', { op, pkg: name });
      pluginOutput.textContent = text;
      setPluginStatus(op === 'add' ? '安装完成，请重启工作台生效' : '卸载完成，请重启工作台生效', 'ok');
    } catch (e) {
      const msg = typeof e === 'string' ? e : (e && e.message) || String(e);
      pluginOutput.textContent = msg;
      setPluginStatus('操作失败，详见下方输出', 'err');
    } finally {
      setPluginBusy(false);
    }
  }
  btnInstall.addEventListener('click', () => runPlugin('add'));
  btnRemove.addEventListener('click', () => runPlugin('remove'));
  pkgEl.addEventListener('keydown', (e) => { if (e.key === 'Enter') runPlugin('add'); });

  // ---------------- 更新 ----------------
  const curVersion = $('curVersion');
  const updateStatus = $('updateStatus');
  (async () => {
    try {
      const st = await invoke('get_shell_state');
      curVersion.textContent = st.dsh_version || '未知';
    } catch (e) { curVersion.textContent = '未知'; }
  })();
  $('btnCheckUpdate').addEventListener('click', async () => {
    updateStatus.textContent = '正在检查…';
    updateStatus.className = 'status';
    try {
      const r = await invoke('check_update_cmd');
      if (r === 'updating') {
        updateStatus.textContent = '发现新版本，已开始更新';
        updateStatus.className = 'status ok';
      } else if (r === 'cancelled') {
        updateStatus.textContent = '已取消更新';
        updateStatus.className = 'status';
      } else {
        updateStatus.textContent = '已是最新版本';
        updateStatus.className = 'status ok';
      }
    } catch (e) {
      updateStatus.textContent = '检查更新失败：' + (e.message || e);
      updateStatus.className = 'status err';
    }
  });

  // ---------------- 卸载 ----------------
  const uninstallStatus = $('uninstallStatus');
  async function runUninstall(wipe) {
    uninstallStatus.textContent = '正在卸载…';
    uninstallStatus.className = 'status';
    [btnUninstallKeep, btnUninstallWipe].forEach((b) => { b.disabled = true; });
    try {
      await invoke('uninstall_run', { wipe });
      // App 即将退出；此处不再刷新
    } catch (e) {
      uninstallStatus.textContent = '卸载未完成：' + (e.message || e);
      uninstallStatus.className = 'status err';
      [btnUninstallKeep, btnUninstallWipe].forEach((b) => { b.disabled = false; });
    }
  }
  $('btnUninstallKeep').addEventListener('click', () => runUninstall(false));
  $('btnUninstallWipe').addEventListener('click', () => runUninstall(true));

  function showNotify(msg, kind) {
    const s = document.querySelector('.panel.active .status') || $('updateStatus') || $('lanStatus') || $('pluginStatus');
    if (s) { s.textContent = msg; s.className = 'status ' + (kind || 'err'); }
  }

  // 初始渲染（网络状态）
  refreshLan();

  // 暴露给主进程：从托盘/命令面板打开指定 Tab
  window.__openShellTab = selectTab;
  try {
    T.event.listen('shell:tab', (ev) => {
      const n = ev.payload;
      if (n && paletteNames[n]) selectTab(n);
    });
  } catch (e) { /* 忽略 */ }
})();
