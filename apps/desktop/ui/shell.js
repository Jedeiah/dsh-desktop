// 壳页 shell.js：工作台(iframe) + 4 个 Tab（工作台 / dsh / 插件 / 关于）
// 全部管理能力经 window.__TAURI__.core.invoke 走真实 IPC（Tauri 只往主 frame
// 注入 __TAURI__，远程 dsh iframe 拿不到，安全面收窄）。
// ⌘K/Ctrl+K 循环切换 Tab；Esc 在管理页返回工作台。
(() => {
  'use strict';
  const $ = (id) => document.getElementById(id);
  const T = window.__TAURI__;
  const invoke = (...a) => T.core.invoke(...a);

  // ---------------- Tab 切换 ----------------
  const TAB_NAMES = ['workbench', 'dsh', 'plugins', 'about'];
  const tabLabels = { workbench: '工作台', dsh: 'dsh', plugins: '插件', about: '关于' };
  const tabs = $('tabs').querySelectorAll('.tab');

  function tabOf(name) {
    if (name === 'workbench') {
      document.querySelectorAll('.panel').forEach((p) => p.classList.remove('active'));
      $('activity-workbench').style.display = 'block';
      return;
    }
    $('activity-workbench').style.display = 'none';
    TAB_NAMES.slice(1).forEach((n) => {
      $('panel-' + n).classList.toggle('active', n === name);
    });
    // 进入管理页时刷新对应数据（安装状态可能已在后台变化）
    if (name === 'dsh') refreshDsh();
    if (name === 'plugins') refreshPlugins();
  }
  function selectTab(name) {
    tabs.forEach((t) => t.classList.toggle('active', t.dataset.tab === name));
    tabOf(name);
  }
  tabs.forEach((t) => t.addEventListener('click', () => selectTab(t.dataset.tab)));

  // ⌘K / Ctrl+K：循环切换；Esc：管理页返回工作台
  window.addEventListener('keydown', (e) => {
    if ((e.metaKey || e.ctrlKey) && (e.key === 'k' || e.key === 'K')) {
      e.preventDefault();
      const cur = [...tabs].find((t) => t.classList.contains('active'));
      const idx = cur ? TAB_NAMES.indexOf(cur.dataset.tab) : 0;
      selectTab(TAB_NAMES[(idx + 1) % TAB_NAMES.length]);
    }
    if (e.key === 'Escape') {
      // 输入框/下拉聚焦时 Esc 不切 Tab（避免误触打断编辑）
      const t = e.target;
      if (t && (t.tagName === 'INPUT' || t.tagName === 'SELECT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return;
      const activePanel = document.querySelector('.panel.active');
      if (activePanel) selectTab('workbench');
    }
  });

  // ---------------- 工作台 iframe ----------------
  const wb = $('workbenchFrame');
  const wbPlaceholder = $('wbPlaceholder');
  const setupView = $('setupView');
  let lastUrl = '';
  let setupActive = false; // 引导视图是否正覆盖工作台

  function showPlaceholder() {
    // 先清内联 display 再取消 hidden（hidden=false 但残留 inline display:none
    // 时仍会隐藏；反之先 hidden=false 再清 display 会出现一帧闪变）
    clearTimeout(placeholderTimer);
    wbPlaceholder.style.display = '';
    wbPlaceholder.hidden = false;
  }
  function hidePlaceholder() {
    // hidden 属性 + 显式 display 双保险（.placeholder 的 display:flex 会覆盖
    // hidden 的默认 display:none；shell.html 已补 [hidden]{display:none!important}）
    wbPlaceholder.hidden = true;
    wbPlaceholder.style.display = 'none';
  }
  let placeholderTimer = null;

  // 占位层是绝对定位覆盖在 iframe 之上：在 iframe 真正绘制完成（onload）前
  // 一直盖住，把"白屏/空白 iframe"阶段用 spinner 遮住。onload 后**延迟淡出**
  // ——dsh web 是 SPA，load 远早于工作台就绪，立刻撤会露出其白屏/与它自带
  // loading 交叠（用户反馈的"卡一下/loading 重叠"）。延迟 900ms 让首帧渲染完成。
  wb.addEventListener('load', () => {
    clearTimeout(placeholderTimer);
    placeholderTimer = setTimeout(() => hidePlaceholder(), 900);
  });
  wb.addEventListener('error', () => showPlaceholder());

  function loadWorkbench(url) {
    if (!url || url === lastUrl) return;
    lastUrl = url;
    // dsh:url 事件 = 工作台就绪（引导安装成功后 boot 会发）→ 收起引导视图
    if (setupActive) hideSetupView();
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

  // ---------------- 首次引导（dsh 闭包未安装） ----------------
  const setupStage = $('setupStage');
  const setupMeta = $('setupMeta');
  const setupProgress = $('setupProgress');
  const setupStageArea = $('setupStageArea');
  const setupError = $('setupError');
  const setupErrTitle = $('setupErrTitle');
  const setupErrMsg = $('setupErrMsg');
  const btnSetupInstall = $('btnSetupInstall');
  const btnSetupRefresh = $('btnSetupRefresh');
  const btnSetupCancel = $('btnSetupCancel');
  const btnSetupRetry = $('btnSetupRetry');
  const setupReg = $('setupReg');
  const setupVer = $('setupVer');
  const setupAdv = $('setupAdv');
  let setupCancelled = false;
  let setupDoneTimer = null; // 安装成功但 dsh:url 未达（boot 失败）时的恢复定时器
  let setupProgressTimer = null; // 安装进度轮询（setup_state_cmd 兜底）

  function showSetupView() {
    setupActive = true;
    hidePlaceholder();
    setupView.hidden = false;
    loadSetupVersions(); // 预填版本下拉（失败仅提示，不阻塞安装主流程）
  }
  function hideSetupView() {
    setupActive = false;
    setupView.hidden = true;
    clearTimeout(setupDoneTimer); // 已切回工作台：取消「启动失败」恢复定时器
    clearInterval(setupProgressTimer); // 进度轮询停止
    if (!lastUrl) showPlaceholder(); // 引导收起但工作台未就绪：恢复占位 spinner
  }
  function setSetupPhase(phase) {
    // phase: 'stage'（进行中）/ 'error'（失败或取消，可重试）
    setupStageArea.hidden = phase !== 'stage';
    setupError.hidden = phase !== 'error';
  }
  function showSetupError(title, msg) {
    setupErrTitle.textContent = title;
    setupErrMsg.textContent = msg;
    setSetupPhase('error');
  }

  async function loadSetupVersions(registry) {
    setupVer.innerHTML = '<option value="">加载中…</option>';
    let list = [];
    try {
      list = await invoke('list_dsh_versions_cmd', { registry: registry || null });
    } catch (e) { /* 网络失败：下方给提示项 */ }
    if (Array.isArray(list) && list.length) {
      setupVer.innerHTML = '';
      list.forEach((v) => {
        const o = document.createElement('option');
        o.value = v;
        o.textContent = 'v' + v;
        setupVer.appendChild(o);
      });
    } else {
      setupVer.innerHTML = '<option value="">未获取到版本列表（检查网络后重试）</option>';
    }
  }

  async function runSetup() {
    const ver = setupVer.value;
    if (!ver) {
      showSetupError('无法开始安装', '版本列表为空。请检查网络连接或更换 Registry 源后重试。');
      return;
    }
    const registry = setupReg.value.trim() || 'https://registry.npmjs.org';
    setupCancelled = false;
    clearTimeout(setupDoneTimer);
    // 进入进行中态：进度条 + 取消按钮出现，安装按钮锁定
    setupProgress.hidden = false;
    btnSetupInstall.disabled = true;
    btnSetupCancel.hidden = false;
    btnSetupCancel.disabled = false;
    btnSetupCancel.textContent = '取消安装';
    setupAdv.open = false;
    setupStage.textContent = '正在连接 registry…';
    setupMeta.textContent = '首次安装约 300MB，请耐心等待';
    // 进度轮询（主通道）：dsh:setup-progress 事件在部分环境不可靠
    // （T.event.listen 曾实测挂起），1s 轮询 setup_state_cmd.progress 兜底。
    let lastProgress = null;
    setupProgressTimer = setInterval(async () => {
      if (!setupActive || setupCancelled) { clearInterval(setupProgressTimer); return; }
      try {
        const st = await invoke('setup_state_cmd');
        if (st.progress && st.progress !== lastProgress) {
          lastProgress = st.progress;
          setupStage.textContent = st.progress;
        }
      } catch (e) { /* 轮询失败忽略，事件通道兜底 */ }
    }, 1000);
    try {
      await invoke('setup_dsh_cmd', { ver, registry });
      clearInterval(setupProgressTimer);
      // 成功：后端 boot 会 emit dsh:url → loadWorkbench 自动切回工作台；
      // 若 4s 内未收到 dsh:url（boot 失败/事件丢失），恢复为可重试状态。
      if (!setupCancelled) {
        setupStage.textContent = '安装完成，正在启动工作台…';
        setupMeta.textContent = '';
        btnSetupCancel.disabled = true;
        setupDoneTimer = setTimeout(() => {
          if (setupActive && !setupCancelled) {
            showSetupError(
              '安装完成但工作台启动失败',
              'dsh 已安装，但工作台未能自动启动。可重试：将先尝试直接拉起工作台，失败则重新安装。'
            );
          }
        }, 4000);
      }
    } catch (e) {
      clearInterval(setupProgressTimer);
      const msg = (e && e.message) || String(e);
      if (setupCancelled) {
        showSetupError('已取消', '安装已取消，可重试或调整高级选项后再次安装。');
      } else {
        showSetupError('安装失败', msg);
      }
    }
  }

  // 高级区：Registry 源右侧「刷新版本」→ 按当前输入值重新获取可用版本
  btnSetupRefresh.addEventListener('click', async () => {
    btnSetupRefresh.disabled = true;
    btnSetupRefresh.textContent = '刷新中…';
    await loadSetupVersions(setupReg.value.trim() || null);
    btnSetupRefresh.disabled = false;
    btnSetupRefresh.textContent = '刷新版本';
  });

  btnSetupInstall.addEventListener('click', () => runSetup());
  btnSetupRetry.addEventListener('click', async () => {
    // 先尝试恢复工作台（适用于「安装完成但启动失败」：dsh 可能已就绪，
    // 仅 dsh:url 事件丢失）；拿不到 URL 再走重新安装。
    for (let i = 0; i < 5; i++) {
      try {
        const url = await invoke('get_dsh_url');
        if (url) { loadWorkbench(url); return; }
      } catch (e) { break; }
      await new Promise((r) => setTimeout(r, 800));
    }
    runSetup();
  });
  setupReg.addEventListener('keydown', (e) => { if (e.key === 'Enter') runSetup(); });

  btnSetupCancel.addEventListener('click', () => {
    setupCancelled = true;
    btnSetupCancel.disabled = true;
    btnSetupCancel.textContent = '正在取消…';
    setupStage.textContent = '正在取消安装…';
    invoke('setup_cancel_cmd').catch(() => {});
  });

  // 进度：后端以阶段文本推送（npm 输出不流式），进度条为不确定态流动动画
  T.event.listen('dsh:setup-progress', (e) => {
    if (setupActive) setupStage.textContent = String(e.payload || '');
  }).catch(() => {});

  // 辅助触发（防竞态兜底）：boot 里未装闭包会发 dsh:need-setup；webview 挂
  // 监听前发出也不怕——初始化 setup_state_cmd 探测是主通道。
  T.event.listen('dsh:need-setup', () => {
    if (!lastUrl && !setupActive) showSetupView();
  }).catch(() => {});

  // ---------------- dsh 页（版本管理） ----------------
  const curVersion = $('curVersion');
  const curBadge = $('curBadge');
  const dshStatus = $('dshStatus');
  const updateSection = $('updateSection');
  const updateVer = $('updateVer');
  const btnUpdateLatest = $('btnUpdateLatest');
  const dshVersionsEl = $('dshVersions');
  const regInput = $('regInput');
  const btnSaveRegistry = $('btnSaveRegistry');
  const regStatus = $('regStatus');
  let dshLatest = null;

  function setDshStatus(text, kind) {
    dshStatus.textContent = text;
    dshStatus.className = 'status ' + (kind || '');
  }

  async function refreshDsh() {
    // 切换 Tab 立即给反馈：先显示 loading，数据到达后渲染（网络慢时不白屏）
    setDshStatus('正在加载…', 'run');
    dshVersionsEl.innerHTML = '';
    const loading = document.createElement('div');
    loading.className = 'empty';
    loading.textContent = '正在获取版本列表…';
    dshVersionsEl.appendChild(loading);
    try {
      const st = await invoke('get_dsh_state');
      // st: { current, latest, versions, installing }
      const current = st.current || '未安装';
      dshLatest = st.latest || null;
      curVersion.textContent = current;
      curBadge.hidden = !current || current === '未安装';

      const hasUpdate = !!dshLatest && dshLatest !== current && current !== '未安装';
      updateSection.hidden = !hasUpdate;
      if (hasUpdate) updateVer.textContent = 'v' + dshLatest;
      btnUpdateLatest.disabled = !!st.installing;
      btnCheckUpdate.disabled = !!st.installing;
      btnCheckUpdate.textContent = st.installing ? '安装中…' : '检查更新';

      renderVersions(st.versions || [], current, dshLatest, !!st.installing);

      if (st.installing) {
        setDshStatus('正在安装新版本…安装完成后工作台自动重启', 'run');
      } else if (hasUpdate) {
        setDshStatus('发现新版本 v' + dshLatest + '，可更新', 'acc');
      } else if (!dshLatest) {
        // LATEST_DSH 为启动时一次查询的缓存；空 = 离线或检查失败，如实提示
        setDshStatus('暂无法确认最新版本（离线或启动时检查失败）', 'warn');
      } else {
        setDshStatus('已是最新版本', 'ok');
      }
    } catch (e) {
      setDshStatus('读取 dsh 状态失败：' + (e.message || e), 'err');
    }
  }

  // 渐进披露：展开态记忆标志（模块级，refreshDsh 重渲染不折叠）
  const versionsExpanded = { value: false };
  function renderVersions(versions, current, latest, installing) {
    dshVersionsEl.innerHTML = '';
    if (!versions.length) {
      const empty = document.createElement('div');
      empty.className = 'empty';
      empty.textContent = '未获取到可用版本（检查网络或 Registry 源）';
      dshVersionsEl.appendChild(empty);
      return;
    }
    // 渐进披露：默认只渲染最近 20 个（dsh 迭代快，版本多了全量渲染既长又噪）；
    // 底部「显示全部 N 个版本」展开剩余。展开态记忆：refreshDsh 重渲染不折叠。
    const VISIBLE = 20;
    const showAll = versionsExpanded.value || versions.length <= VISIBLE;
    const slice = showAll ? versions : versions.slice(0, VISIBLE);
    const mkRow = (v) => {
      const row = document.createElement('div');
      row.className = 'list-row';

      const grow = document.createElement('div');
      grow.className = 'grow';
      const title = document.createElement('span');
      title.className = 'row-title mono';
      title.textContent = 'v' + v;
      grow.appendChild(title);
      if (latest && v === latest) {
        const b = document.createElement('span');
        b.className = 'badge badge-latest';
        b.textContent = '最新';
        grow.appendChild(b);
      }
      if (v === current) {
        const b = document.createElement('span');
        b.className = 'badge badge-current';
        b.textContent = '当前';
        grow.appendChild(b);
      }
      row.appendChild(grow);

      const btn = document.createElement('button');
      btn.className = 'sm';
      if (v === current) {
        btn.textContent = '当前版本';
        btn.disabled = true;
      } else {
        btn.textContent = '安装';
        btn.addEventListener('click', () => { btn.disabled = true; updateDsh(v); });
        btn.disabled = installing;
      }
      row.appendChild(btn);
      return row;
    };
    slice.forEach((v) => dshVersionsEl.appendChild(mkRow(v)));
    if (!showAll) {
      const more = document.createElement('button');
      more.className = 'ghost sm more-versions';
      more.textContent = '显示全部 ' + versions.length + ' 个版本';
      more.addEventListener('click', () => {
        versionsExpanded.value = true;
        dshVersionsEl.innerHTML = '';
        versions.forEach((v) => dshVersionsEl.appendChild(mkRow(v)));
      });
      dshVersionsEl.appendChild(more);
    }
  }

  async function updateDsh(ver) {
    // 点击即锁定全部安装入口（防连点/其他版本并发）：版本行按钮、更新到最新、
    // 检查更新（hero 右侧不空）。finally 里 refreshDsh 按后端 installing 恢复。
    setDshStatus('正在安装 dsh v' + ver + '…', 'run');
    btnUpdateLatest.disabled = true;
    btnCheckUpdate.disabled = true;
    btnCheckUpdate.textContent = '安装中…';
    dshVersionsEl.querySelectorAll('button').forEach((b) => { b.disabled = true; });
    // 进度显示（复用引导页的 SETUP_PROGRESS 轮询通道）：安装中每秒拉一次状态。
    let lastProgress = null;
    const progressTimer = setInterval(async () => {
      try {
        const st = await invoke('setup_state_cmd');
        if (st.progress && st.progress !== lastProgress) {
          lastProgress = st.progress;
          setDshStatus(st.progress, 'run');
        }
      } catch (e) { /* 轮询失败忽略 */ }
    }, 1000);
    try {
      await invoke('update_dsh_cmd', { ver });
      clearInterval(progressTimer);
      // 后端安装成功 → 自动重启工作台（dsh:url 事件驱动 iframe 重载）
      setDshStatus('安装完成，工作台正在重启…', 'ok');
    } catch (e) {
      clearInterval(progressTimer);
      setDshStatus('安装失败：' + (e.message || e), 'err');
    } finally {
      refreshDsh(); // 复位 installing 状态并重渲染列表
    }
  }

  $('btnCheckUpdate').addEventListener('click', () => refreshDsh());
  btnUpdateLatest.addEventListener('click', () => { if (dshLatest) updateDsh(dshLatest); });

  async function refreshRegistry() {
    try {
      const st = await invoke('get_shell_state');
      regInput.value = st.registry || 'https://registry.npmjs.org';
    } catch (e) {
      regStatus.textContent = '读取 Registry 源失败：' + (e.message || e);
      regStatus.className = 'status err';
    }
  }

  async function saveRegistry() {
    const val = regInput.value.trim();
    if (!val) {
      regStatus.textContent = 'Registry 源不能为空';
      regStatus.className = 'status err';
      return;
    }
    btnSaveRegistry.disabled = true;
    regStatus.textContent = '正在保存…';
    regStatus.className = 'status run';
    try {
      await invoke('save_registry_cmd', { registry: val });
      regStatus.textContent = '已保存，后续安装与更新将使用该源';
      regStatus.className = 'status ok';
      await refreshRegistry(); // 回显规范化后的地址（如去掉尾部斜杠）
    } catch (e) {
      regStatus.textContent = '保存失败：' + (e.message || e);
      regStatus.className = 'status err';
    } finally {
      btnSaveRegistry.disabled = false;
    }
  }
  btnSaveRegistry.addEventListener('click', saveRegistry);
  regInput.addEventListener('keydown', (e) => { if (e.key === 'Enter') saveRegistry(); });

  // ---------------- 插件页 ----------------
  const pkgEl = $('pkg');
  const btnInstall = $('btnInstall');
  const btnRemove = $('btnRemove');
  const pluginStatus = $('pluginStatus');
  const pluginOutput = $('pluginOutput');
  const pluginListEl = $('pluginList');
  const pluginCountEl = $('pluginCount');
  const pluginEmpty = $('pluginEmpty');
  const logClear = $('logClear');
  let pluginBusy = false;

  function setPluginStatus(text, kind) {
    pluginStatus.textContent = text;
    pluginStatus.className = 'status ' + (kind || '');
  }
  function setPluginBusy(busy) {
    pluginBusy = busy;
    btnInstall.disabled = busy;
    btnRemove.disabled = busy;
    pkgEl.disabled = busy;
  }

  async function refreshPlugins() {
    try {
      const list = await invoke('plugin_list_cmd');
      const arr = Array.isArray(list) ? list : [];
      pluginListEl.innerHTML = '';
      pluginCountEl.textContent = arr.length + ' 个插件';
      pluginEmpty.hidden = arr.length > 0;
      arr.forEach((p) => {
        const row = document.createElement('div');
        row.className = 'list-row';

        const grow = document.createElement('div');
        grow.className = 'grow';
        const title = document.createElement('span');
        title.className = 'row-title mono';
        title.textContent = p.name;
        grow.appendChild(title);
        if (p.installed) {
          const b = document.createElement('span');
          b.className = 'badge badge-installed';
          b.textContent = '已安装';
          grow.appendChild(b);
        }
        row.appendChild(grow);

        const meta = document.createElement('span');
        meta.className = 'row-meta mono';
        meta.textContent = p.version ? 'v' + p.version : '';
        row.appendChild(meta);
        pluginListEl.appendChild(row);
      });
    } catch (e) {
      pluginCountEl.textContent = '读取失败';
      setPluginStatus('读取插件列表失败：' + (e.message || e), 'err');
    }
  }

  // 实时输出：插件构建脚本的行级推送（后端已自动重启工作台）
  T.event.listen('dsh:plugin-output', (e) => {
    pluginOutput.hidden = false;
    pluginOutput.textContent += String(e.payload || '') + '\n';
    pluginOutput.scrollTop = pluginOutput.scrollHeight;
  }).catch(() => {});
  logClear.addEventListener('click', () => { pluginOutput.textContent = ''; });
  logClear.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); pluginOutput.textContent = ''; }
  });

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
      setPluginStatus('已完成，工作台正在重启…', 'ok');
    } catch (e) {
      const msg = typeof e === 'string' ? e : (e && e.message) || String(e);
      pluginOutput.textContent = msg;
      setPluginStatus('操作失败，详见下方输出', 'err');
    } finally {
      setPluginBusy(false);
      refreshPlugins();
    }
  }
  btnInstall.addEventListener('click', () => runPlugin('add'));
  btnRemove.addEventListener('click', () => runPlugin('remove'));
  pkgEl.addEventListener('keydown', (e) => { if (e.key === 'Enter') runPlugin('add'); });

  // ---------------- 关于页（App 更新 / 卸载） ----------------
  const appVersion = $('appVersion');
  const appStatus = $('appStatus');
  const btnCheckApp = $('btnCheckApp');
  const btnAppDownload = $('btnAppDownload');
  let appLatest = null;

  function setAppStatus(text, kind) {
    appStatus.textContent = text;
    appStatus.className = 'status about-status ' + (kind || '');
  }

  async function refreshApp() {
    try {
      const st = await invoke('get_shell_state');
      appVersion.textContent = 'v' + st.app_version;
    } catch (e) {
      appVersion.textContent = '未知';
    }
  }

  $('btnCheckApp').addEventListener('click', async () => {
    btnCheckApp.disabled = true;
    setAppStatus('正在检查更新…');
    try {
      const v = await invoke('check_app_update_cmd');
      if (v) {
        appLatest = v;
        btnAppDownload.disabled = false;
        btnAppDownload.textContent = '下载并安装 v' + v;
        setAppStatus('发现新版本 v' + v, 'acc');
      } else {
        appLatest = null;
        btnAppDownload.disabled = true;
        btnAppDownload.textContent = '下载并安装更新';
        setAppStatus('已是最新版本', 'ok');
      }
    } catch (e) {
      setAppStatus('检查更新失败：' + (e.message || e), 'err');
    } finally {
      btnCheckApp.disabled = false;
    }
  });

  btnAppDownload.addEventListener('click', async () => {
    btnAppDownload.disabled = true;
    setAppStatus('正在下载并安装更新…安装完成后应用将自动重启');
    try {
      await invoke('app_update_cmd');
      // 成功即退出当前实例（安装器/新版负责启动）；无需刷新
    } catch (e) {
      setAppStatus('更新失败：' + (e.message || e), 'err');
      btnAppDownload.disabled = !appLatest;
    }
  });

  $('btnOpenReleases').addEventListener('click', () => {
    invoke('open_browser_cmd').catch((e) => setAppStatus('打开下载页失败：' + (e.message || e), 'err'));
  });

  // ---------------- 卸载（两档 + 行内确认） ----------------
  const btnUninstallKeep = $('btnUninstallKeep');
  const btnUninstallWipe = $('btnUninstallWipe');
  const uninstallStatus = $('uninstallStatus');

  function runUninstall(wipe) {
    uninstallStatus.textContent = '正在卸载…';
    uninstallStatus.className = 'status';
    [btnUninstallKeep, btnUninstallWipe].forEach((b) => { b.disabled = true; });
    invoke('confirm_uninstall_cmd', { wipe })
      .then(() => {
        // 成功返回 = 用户取消（确认后真正卸载会退出 App，此处复位仅影响取消路径）
        [btnUninstallKeep, btnUninstallWipe].forEach((b) => { b.disabled = false; });
        uninstallStatus.textContent = '';
        uninstallStatus.className = 'status';
      })
      .catch((e) => {
        uninstallStatus.textContent = '卸载未完成：' + (e.message || e);
        uninstallStatus.className = 'status err';
        [btnUninstallKeep, btnUninstallWipe].forEach((b) => { b.disabled = false; });
      });
  }
  btnUninstallKeep.addEventListener('click', () => runUninstall(false));
  btnUninstallWipe.addEventListener('click', () => runUninstall(true));

  // ---------------- 初始渲染 + 引导探测 ----------------
  (async () => {
    // 引导状态探测（主通道）：current 为 None → 显示引导视图。
    // dsh:need-setup 事件可能在 webview 挂监听前发出而丢失，仅作辅助。
    try {
      const st = await invoke('setup_state_cmd');
      if (!st.current) showSetupView();
    } catch (e) { /* 探测失败：依赖 dsh:need-setup 事件与工作台轮询兜底 */ }

    // 各页初始数据
    refreshDsh();
    refreshRegistry();
    refreshApp();
    refreshPlugins();
  })();

  // shell:tab 事件监听已随托盘「管理台」移除（无 emit 方），删除死代码
})();
