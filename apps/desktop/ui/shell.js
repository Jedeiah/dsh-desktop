// 壳页 shell.js v4（无缝壳体）：满屏工作台基底 + 36px 可折叠顶栏 + ⌘K 命令面板
// + 右侧管理抽屉（dsh/插件/关于 分段）+ 全局任务中心 + Toast + 确认弹窗。
// 工作台 = 原生 child webview（Rust workbench 模块管理），壳页不放 iframe。
// 全部管理能力经 window.__TAURI__.core.invoke 走真实 IPC（Tauri 只往主 frame
// 注入 __TAURI__，dsh 工作台 webview 拿不到，安全面收窄）。
// 命令名/参数名与事件名沿用 v3（后端 25 command / 4 event 零改动）。
(() => {
  'use strict';
  const $ = (id) => document.getElementById(id);
  const T = window.__TAURI__;
  const invoke = (...a) => T.core.invoke(...a);

  // ---------------- 图标表（v4 设计稿同款，线性 1.5px） ----------------
  const ICON = {
    check: '<svg class="ic" viewBox="0 0 20 20"><path d="M5 10.5l3.2 3.2L15 6.5"/></svg>',
    alert: '<svg class="ic" viewBox="0 0 20 20"><circle cx="10" cy="10" r="7.5"/><path d="M10 6v5M10 13.5v.5"/></svg>',
    trash: '<svg class="ic" viewBox="0 0 20 20"><path d="M4 6h12M8.5 6V4.5h3V6"/><path d="M6 6l.7 8.8a1.6 1.6 0 0 0 1.6 1.5h3.4a1.6 1.6 0 0 0 1.6-1.5L14 6"/></svg>',
    refresh: '<svg class="ic" viewBox="0 0 20 20"><path d="M15.5 8A6 6 0 1 0 16 12"/><path d="M16 4.5V8h-3.5"/></svg>',
    external: '<svg class="ic" viewBox="0 0 20 20"><path d="M11 4h5v5M16 4l-6.5 6.5"/><path d="M14 12.5V15a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 15V7.5A1.5 1.5 0 0 1 5 6h2.5"/></svg>',
    terminal: '<svg class="ic" viewBox="0 0 20 20"><rect x="2.5" y="4" width="15" height="12" rx="2"/><path d="M6 8.5l2 2-2 2M10.5 12.5h3.5"/></svg>',
    layers: '<svg class="ic" viewBox="0 0 20 20"><path d="M10 2.5l7 3.5-7 3.5-7-3.5z"/><path d="M3 10.5l7 3.5 7-3.5"/></svg>',
    info: '<svg class="ic" viewBox="0 0 20 20"><circle cx="10" cy="10" r="7.5"/><path d="M10 9v4.5M10 6.2v.5"/></svg>',
    folder: '<svg class="ic" viewBox="0 0 20 20"><path d="M2.5 5.5A1.5 1.5 0 0 1 4 4h3.5l1.5 2h7A1.5 1.5 0 0 1 17.5 7.5v7A1.5 1.5 0 0 1 16 16H4a1.5 1.5 0 0 1-1.5-1.5z"/></svg>',
    chevron: '<svg class="ic" viewBox="0 0 20 20"><path d="M7.5 5l5 5-5 5"/></svg>',
  };

  // ---------------- HTML 转义（toast/任务中心/版本表等外部数据进入 innerHTML 前必须过） ----------------
  // 壳页持有 __TAURI__（IPC 权限面），registry 版本键/插件输出/错误串都是不可信数据，
  // 不能直接拼进 innerHTML（详见代码审查 #1）；面板内静态常量（REGIONS/COMMANDS 图标）
  // 是可信代码，不经此函数。
  function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  }

  // ---------------- Toast（短反馈，2.8s 消失） ----------------
  function toast(msg, kind) {
    const el = document.createElement('div');
    el.className = 'toast';
    const iconCls = kind === 'ok' ? 't-ok' : kind === 'err' ? 't-err' : 't-acc';
    const icon = kind === 'err' ? ICON.alert : kind === 'ok' ? ICON.check : ICON.info;
    el.innerHTML = '<span class="' + iconCls + '">' + icon + '</span><span>' + escapeHtml(msg) + '</span>';
    $('toasts').appendChild(el);
    setTimeout(() => { el.classList.add('leaving'); setTimeout(() => el.remove(), 220); }, 2800);
  }

  // ---------------- 任务中心（长任务的唯一真相在顶栏胶囊） ----------------
  const tasks = [];
  function taskStart(id, label) {
    const i = tasks.findIndex((t) => t.id === id);
    if (i >= 0) tasks.splice(i, 1);
    const t = { id, label, state: 'run', detail: '' };
    tasks.unshift(t);
    renderTasks();
    return t;
  }
  function taskUpdate(id, detail) {
    const t = tasks.find((x) => x.id === id);
    if (t) { t.detail = detail; renderTasks(); }
  }
  function taskFinish(id, state, detail) {
    const t = tasks.find((x) => x.id === id);
    if (!t) return;
    t.state = state; t.detail = detail || '';
    renderTasks();
    // 完成 5s 后归档（避免胶囊常驻历史任务）
    setTimeout(() => {
      const i = tasks.findIndex((x) => x.id === id && x.state !== 'run');
      if (i >= 0) { tasks.splice(i, 1); renderTasks(); }
    }, 5000);
  }
  function renderTasks() {
    const pill = $('taskPill');
    const list = $('taskList');
    if (!tasks.length) { pill.hidden = true; $('taskPop').hidden = true; return; }
    const head = tasks[0];
    pill.hidden = false;
    pill.innerHTML = head.state === 'run'
      ? '<span class="spinner"></span><span>' + escapeHtml(head.label) + (head.detail ? ' · ' + escapeHtml(head.detail) : '') + '</span>'
      : (head.state === 'ok'
          ? '<span class="tick">' + ICON.check + '</span><span>' + escapeHtml(head.detail || head.label) + '</span>'
          : '<span class="warn">' + ICON.alert + '</span><span>' + escapeHtml(head.detail || head.label + ' 未完成') + '</span>');
    list.innerHTML = '';
    tasks.forEach((t) => {
      const row = document.createElement('div');
      row.className = 'task-item';
      const icon = t.state === 'run' ? '<span class="spinner"></span>'
        : t.state === 'ok' ? '<span style="color:var(--success)">' + ICON.check + '</span>'
        : '<span style="color:var(--danger)">' + ICON.alert + '</span>';
      row.innerHTML = icon + '<div><div class="ti-label">' + escapeHtml(t.label) + '</div>' +
        (t.detail ? '<div class="ti-detail">' + escapeHtml(t.detail) + '</div>' : '') + '</div>';
      list.appendChild(row);
    });
  }
  $('taskPill').addEventListener('click', (e) => {
    e.stopPropagation();
    const pop = $('taskPop');
    pop.hidden = !pop.hidden;
  });
  document.addEventListener('click', () => { $('taskPop').hidden = true; });

  // ---------------- 工作台：占位 / 就绪（child webview 由 Rust workbench 管理） ----------------
  // 壳页只负责：占位层显隐、隐藏/显示 webview 触发、刷新触发。
  const startupView = $('startupView');
  let setupActive = false; // 引导浮层是否正覆盖工作台
  let workbenchReady = false; // 工作台是否已完成首次加载（撤占位依据）
  let placeholderTimer = null;

  function showPlaceholder() {
    // 先清内联 display 再取消 hidden（hidden=false 但残留 inline display:none
    // 时仍会隐藏；反之先 hidden=false 再清 display 会出现一帧闪变）
    clearTimeout(placeholderTimer);
    startupView.style.display = '';
    startupView.hidden = false;
  }
  function hidePlaceholder() {
    // hidden 属性 + 显式 display 双保险（.startup 的 display:flex 覆盖了
    // hidden 的默认 display:none；shell.html 已补 [hidden]{display:none!important}）
    startupView.hidden = true;
    startupView.style.display = 'none';
  }

  // 工作台首帧已绘制（Rust 每次 page-load Finished 都会 emit；重装/换端口后
  // 重新触发）：撤占位 + 收引导浮层。幂等——事件与轮询可能各触发一次。
  function onWorkbenchReady() {
    workbenchReady = true;
    clearTimeout(placeholderTimer);
    // 短暂延迟后撤占位，避免与 WebView 首帧竞争出现闪白
    placeholderTimer = setTimeout(() => hidePlaceholder(), 300);
    if (setupActive) hideSetupView();
  }

  // 就绪信号：事件常驻（含重装/换端口后的二次就绪）+ 轮询兜底（命令查询）。
  // 本项目 T.event.listen 曾实测丢失/挂起，事件不可靠时靠轮询。
  T.event.listen('workbench:ready', onWorkbenchReady).catch(() => {});
  (async () => {
    try {
      if (await invoke('workbench_ready_cmd')) { onWorkbenchReady(); return; }
    } catch (e) { /* 命令不可用：仅靠事件 */ }
    for (let i = 0; i < 90; i++) {
      await new Promise((r) => setTimeout(r, 1000));
      try {
        if (await invoke('workbench_ready_cmd')) { onWorkbenchReady(); return; }
      } catch (e) { /* 单次失败忽略 */ }
    }
  })();

  // ---------------- 品牌：单击刷新，双击系统浏览器打开（200ms 延时消歧） ----------------
  let brandTimer = null;
  $('brand').addEventListener('click', () => {
    clearTimeout(brandTimer);
    brandTimer = setTimeout(() => invoke('workbench_reload_cmd').catch(() => {}), 200);
  });
  $('brand').addEventListener('dblclick', () => {
    clearTimeout(brandTimer);
    invoke('open_workbench_url_cmd').catch(() => {});
    toast('已在浏览器打开工作台地址');
  });

  // ---------------- 顶栏折叠（状态记忆；Rust 几何联动 workbench_set_collapsed_cmd） ----------------
  function setChromeCollapsed(v) {
    document.body.classList.toggle('chrome-collapsed', v);
    $('chromeRestore').hidden = !v;
    try { localStorage.setItem('chromeCollapsed', v ? '1' : '0'); } catch (e) { /* 忽略 */ }
    invoke('workbench_set_collapsed_cmd', { collapsed: v }).catch(() => {});
  }
  $('btnCollapseChrome').addEventListener('click', () => setChromeCollapsed(true));
  $('chromeRestore').addEventListener('click', () => setChromeCollapsed(false));
  try {
    setChromeCollapsed(localStorage.getItem('chromeCollapsed') === '1' ? true
      : localStorage.getItem('tabsCollapsed') === '1');
  } catch (e) { setChromeCollapsed(false); }
  // 启动还原时若顶栏处于折叠（上次误触 / 遗留状态），4s 后提示展开入口——
  // 否则用户看到「管理那一行不见了」却不知道去哪里找回（顶部中央小把手）。
  setTimeout(() => {
    try {
      if (localStorage.getItem('chromeCollapsed') === '1' || localStorage.getItem('tabsCollapsed') === '1') {
        toast('顶部导航栏已收起：点窗口顶部中央的小把手即可展开');
      }
    } catch (e) { /* 忽略 */ }
  }, 4000);

  // ---------------- 区域 / 抽屉 ----------------
  const REGIONS = [
    { id: 'workbench', label: '工作台', hint: '关闭浮层，回到 dsh 工作台', icon: ICON.layers },
    { id: 'dsh', label: 'dsh 版本', hint: '版本、更新与 Registry 源', icon: ICON.terminal },
    { id: 'plugins', label: '插件', hint: '安装 / 卸载插件，查看输出', icon: ICON.folder },
    { id: 'about', label: '关于', hint: 'App 版本、更新与卸载', icon: ICON.info },
  ];
  const COMMANDS = [
    { id: 'refresh', label: '刷新工作台', hint: '重新加载 dsh 工作台', icon: ICON.refresh, run: () => invoke('workbench_reload_cmd').catch(() => {}) },
    { id: 'openBrowser', label: '在浏览器打开工作台', hint: '用系统默认浏览器打开当前 dsh 地址', icon: ICON.external, run: () => { invoke('open_workbench_url_cmd').catch(() => {}); toast('已在浏览器打开工作台地址'); } },
    { id: 'checkDsh', label: '检查 dsh 更新', hint: '立即检查 dsh 运行时新版本', icon: ICON.terminal, run: () => { openDrawer('dsh'); checkDsh(); } },
    { id: 'checkApp', label: '检查应用更新', hint: '检查 DeepSeek Harness Desktop 更新', icon: ICON.info, run: () => { openDrawer('about'); checkApp(); } },
    { id: 'toggleChrome', label: '收起导航栏', hint: '折叠顶栏以扩展工作区', icon: ICON.chevron, run: () => setChromeCollapsed(true) },
  ];
  let region = 'workbench';

  function openDrawer(section) {
    region = section;
    const drawer = $('drawer');
    drawer.querySelectorAll('.drawer-section').forEach((s) => s.classList.toggle('active', s.dataset.section === section));
    drawer.querySelectorAll('.segmented button').forEach((b) => {
      const on = b.dataset.section === section;
      b.classList.toggle('active', on);
      b.setAttribute('aria-selected', String(on));
    });
    drawer.classList.add('open');
    // 抽屉打开期间禁用「收起导航栏」：折叠按钮紧邻抽屉关闭按钮、都是右上角，
    // 极易误触导致整个顶栏消失（用户多次踩坑）。折叠仍可在收起抽屉后/⌘K
    // 命令面板里进行。
    $('btnCollapseChrome').disabled = true;
    // 工作台是原生 webview，盖在所有 HTML 之上：抽屉打开必须显式隐藏
    if (section !== 'workbench') invoke('hide_workbench_cmd').catch(() => {});
    // 切到该分段时刷新数据（安装/插件状态可能已在后台变化）
    if (section === 'dsh') refreshDsh();
    if (section === 'plugins') refreshPlugins();
    if (section === 'about') refreshApp();
  }
  function closeDrawer() {
    $('drawer').classList.remove('open');
    region = 'workbench';
    $('btnCollapseChrome').disabled = false;
    // 等抽屉收回动画（0.2s transition）播完再恢复工作台：立即恢复会让工作台
    // 突然盖住还在滑动中的抽屉，观感变成「收回没有动效、一下消失」
    setTimeout(() => invoke('show_workbench_cmd').catch(() => {}), 260);
  }
  function goRegion(id) {
    if (id === 'workbench') closeDrawer();
    else openDrawer(id);
  }
  $('drawerClose').addEventListener('click', closeDrawer);
  $('drawer').querySelectorAll('.segmented button').forEach((b) => {
    b.addEventListener('click', () => openDrawer(b.dataset.section));
  });

  // ---------------- 命令面板 ----------------
  let paletteOpen = false, paletteSel = 0, paletteItems = [];
  function openPalette() {
    paletteOpen = true;
    $('paletteOverlay').hidden = false;
    // 命令面板是居中浮层，与工作台区域重叠；原生工作台 webview 盖在所有 HTML
    // 之上（macOS 独立 NSWindow），必须像抽屉一样显式隐藏，否则面板被盖住
    // 看不见（用户反馈「点管理没出现操作页面」的唯一原因）。
    invoke('hide_workbench_cmd').catch(() => {});
    $('paletteInput').value = '';
    paletteSel = 0;
    renderPalette('');
    $('paletteInput').focus();
  }
  function closePalette() {
    paletteOpen = false;
    $('paletteOverlay').hidden = true;
    // 恢复工作台；仅当随后会开出抽屉时跳过（runPaletteItem → goRegion 由
    // openDrawer 再隐藏，两次 invoke 按发出顺序执行，最终态正确）。
    if (!$('drawer').classList.contains('open')) invoke('show_workbench_cmd').catch(() => {});
  }
  function renderPalette(q) {
    q = (q || '').trim().toLowerCase();
    const regions = REGIONS.filter((r) => !q || (r.label + r.hint).toLowerCase().includes(q));
    const cmds = COMMANDS.filter((c) => !q || (c.label + c.hint).toLowerCase().includes(q));
    paletteItems = regions.map((r) => ({ kind: 'region', ...r })).concat(cmds.map((c) => ({ kind: 'cmd', ...c })));
    const list = $('paletteList');
    list.innerHTML = '';
    if (!paletteItems.length) {
      list.innerHTML = '<div class="palette-empty">没有匹配的区域或命令</div>';
      return;
    }
    if (paletteSel >= paletteItems.length) paletteSel = 0;
    let lastKind = null;
    paletteItems.forEach((it, i) => {
      if (it.kind !== lastKind) {
        lastKind = it.kind;
        const g = document.createElement('div');
        g.className = 'palette-group';
        g.textContent = it.kind === 'region' ? '区域' : '命令';
        list.appendChild(g);
      }
      const row = document.createElement('div');
      row.className = 'palette-item' + (i === paletteSel ? ' active' : '');
      row.innerHTML = it.icon + '<div class="pi-text"><div class="pi-label">' + it.label +
        '</div><div class="pi-hint">' + it.hint + '</div></div>';
      row.addEventListener('mouseenter', () => {
        paletteSel = i;
        // 只切换高亮，不重建列表（重建会让指针下的 DOM 抖动）
        list.querySelectorAll('.palette-item').forEach((el, j) => el.classList.toggle('active', j === i));
      });
      row.addEventListener('click', () => runPaletteItem(i));
      list.appendChild(row);
    });
  }
  function runPaletteItem(i) {
    const it = paletteItems[i];
    if (!it) return;
    closePalette();
    if (it.kind === 'region') goRegion(it.id);
    else if (typeof it.run === 'function') it.run();
  }
  // ⌘K：未打开 → 打开面板；已打开 → 在「区域」间循环移动选中项
  function cycleRegionSelection() {
    const regionCount = paletteItems.filter((x) => x.kind === 'region').length;
    if (!regionCount) return;
    paletteSel = paletteSel < regionCount - 1 ? paletteSel + 1 : 0;
    renderPalette($('paletteInput').value);
  }
  $('paletteInput').addEventListener('input', function () { paletteSel = 0; renderPalette(this.value); });
  $('paletteInput').addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); paletteSel = (paletteSel + 1) % paletteItems.length; renderPalette($('paletteInput').value); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); paletteSel = (paletteSel - 1 + paletteItems.length) % paletteItems.length; renderPalette($('paletteInput').value); }
    else if (e.key === 'Enter') { e.preventDefault(); runPaletteItem(paletteSel); }
    else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closePalette(); }
  });
  $('paletteOverlay').addEventListener('click', (e) => { if (e.target === $('paletteOverlay')) closePalette(); });
  $('btnManage').addEventListener('click', openPalette);

  // ---------------- 确认弹窗（modal，供插件卸载/版本操作/更新确认复用） ----------------
  const modalEl = $('modal');
  let modalAccept = null;
  function openModal(opts) {
    $('modalTitle').textContent = opts.title || '确认操作';
    $('modalMsg').textContent = opts.message || '';
    $('modalIcon').className = 'modal-icon' + (opts.danger ? ' danger' : '');
    $('modalIcon').innerHTML = opts.danger ? ICON.trash : ICON.alert;
    $('modalOk').textContent = opts.okLabel || '确定';
    $('modalOk').className = 'btn' + (opts.danger ? ' danger' : '');
    $('modalNo').textContent = opts.noLabel || '取消';
    modalAccept = opts.onAccept || null;
    modalEl.hidden = false;
    $('modalOk').focus();
  }
  function closeModal() { modalEl.hidden = true; modalAccept = null; }
  $('modalNo').addEventListener('click', closeModal);
  $('modalClose').addEventListener('click', closeModal);
  modalEl.addEventListener('click', (e) => { if (e.target === modalEl) closeModal(); });
  $('modalOk').addEventListener('click', () => {
    const fn = modalAccept;
    closeModal();
    if (fn) fn();
  });

  // ---------------- 全局快捷键 ----------------
  window.addEventListener('keydown', (e) => {
    const t = e.target;
    const typing = t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable);

    // ⌘C：复制选中文字（输入框内不劫持；工作台 webview 内的 keydown 不会冒泡到壳页）
    if ((e.metaKey || e.ctrlKey) && (e.key === 'c' || e.key === 'C')) {
      if (typing) return;
      const sel = document.getSelection();
      const text = sel ? sel.toString() : '';
      if (!text) return;
      e.preventDefault();
      navigator.clipboard.writeText(text).then(() => toast('已复制', 'ok')).catch(() => {});
      return;
    }
    // ⌘K：命令面板 / 面板内循环区域
    if ((e.metaKey || e.ctrlKey) && (e.key === 'k' || e.key === 'K')) {
      e.preventDefault();
      if (paletteOpen) cycleRegionSelection();
      else openPalette();
      return;
    }
    // ⌘1–⌘4：直达区域
    if ((e.metaKey || e.ctrlKey) && ['1', '2', '3', '4'].includes(e.key)) {
      e.preventDefault();
      goRegion(REGIONS[Number(e.key) - 1].id);
      return;
    }
    // Esc：面板 → 抽屉 → 确认弹窗 逐级关闭
    if (e.key === 'Escape') {
      if (paletteOpen) { closePalette(); return; }
      if ($('drawer').classList.contains('open')) { closeDrawer(); return; }
      if (!modalEl.hidden) { closeModal(); return; }
    }
  });

  // ========================================================================
  // 首次引导（dsh 闭包未安装）
  // ========================================================================
  const setupOverlay = $('setupOverlay');
  const setupStage = $('setupStage');
  const setupMeta = $('setupMeta');
  const setupProgress = $('setupProgress');
  const setupStageArea = $('setupStageArea');
  const setupError = $('setupError');
  const setupDone = $('setupDone');
  const setupDoneVer = $('setupDoneVer');
  const setupErrTitle = $('setupErrTitle');
  const setupErrMsg = $('setupErrMsg');
  const btnSetupInstall = $('btnSetupInstall');
  const btnSetupCancel = $('btnSetupCancel');
  const btnSetupRetry = $('btnSetupRetry');
  const setupReg = $('setupReg');
  const btnRefreshVer = $('btnRefreshVer');
  const setupVerTrigger = $('setupVerTrigger');
  const setupVerLabel = $('setupVerLabel');
  const setupVerMenu = $('setupVerMenu');
  const setupVerShow = $('setupVerShow');
  const verManual = $('verManual');
  const setupAdv = $('setupAdv');
  const btnEnterWorkbench = $('btnEnterWorkbench');
  let setupVerValue = ''; // 自定义下拉当前选中版本（替代原生 select.value）
  let setupCancelled = false;
  let setupProgressTimer = null; // 安装进度轮询（setup_state_cmd 兜底）
  let waitBusyReset = false; // 撞 BUSY 分支：等后端收尾结束后回初始页
  let setupFetchCount = 0;
  let setupLastProgress = null; // 模块级去重：事件与轮询双通道共用，防同一行双计
  let setupRunId = 0; // 安装运行令牌：取消/失败的旧 run 不得覆盖新 run 的 UI
  let setupStarting = false; // 预检/启动期防重入（连点会并发两个 runSetup → 轮询 timer 泄漏）

  function showSetupView() {
    setupActive = true;
    hidePlaceholder();
    setupOverlay.hidden = false;
    loadSetupVersions(); // 预填版本下拉（失败仅提示，不阻塞安装主流程）
  }
  function hideSetupView() {
    setupActive = false;
    setupOverlay.hidden = true;
    clearInterval(setupProgressTimer); // 进度轮询停止
    if (!workbenchReady) showPlaceholder(); // 引导收起但工作台未就绪：恢复占位 spinner
  }
  function setSetupPhase(phase) {
    // phase: 'stage'（进行中）/ 'error'（失败或取消，可重试）/ 'done'（完成）
    setupStageArea.hidden = phase !== 'stage';
    setupError.hidden = phase !== 'error';
    setupDone.hidden = phase !== 'done';
  }
  function showSetupError(title, msg) {
    setupErrTitle.textContent = title;
    setupErrMsg.textContent = msg;
    setSetupPhase('error');
  }

  // 主区「将安装版本 vX」联动：selectSetupVersion 与 verManual 手输都更新它
  function syncSetupVerDisplay(v) {
    setupVerShow.textContent = 'v' + v;
  }

  async function loadSetupVersions(registry) {
    setupVerLabel.textContent = '加载中…';
    let list = [];
    try {
      list = await invoke('list_dsh_versions_cmd', { registry: registry || null });
    } catch (e) { /* 网络失败：下方给提示项 */ }
    if (Array.isArray(list) && list.length) {
      // 自绘下拉（沿用既有实现——Windows 弹出列表无法 CSS 定制，自绘统一 mac/win）
      setupVerMenu.innerHTML = '';
      list.forEach((v) => {
        const li = document.createElement('li');
        li.className = 'cselect-option';
        li.setAttribute('role', 'option');
        li.textContent = 'v' + v;
        li.dataset.value = v;
        li.addEventListener('click', () => selectSetupVersion(v));
        setupVerMenu.appendChild(li);
      });
      // 与原生 select 行为一致：默认选中第一个（最新版本）
      selectSetupVersion(list[0], true);
    } else {
      setupVerMenu.innerHTML = '';
      const li = document.createElement('li');
      li.className = 'cselect-option cselect-empty';
      li.textContent = '未获取到版本列表（检查网络后重试）';
      setupVerMenu.appendChild(li);
      setupVerLabel.textContent = '未获取到版本列表';
    }
  }

  function selectSetupVersion(v, silent) {
    setupVerValue = v;
    setupVerLabel.textContent = 'v' + v;
    syncSetupVerDisplay(v);
    if (!silent) closeSetupVerMenu();
  }
  function closeSetupVerMenu() {
    setupVerMenu.hidden = true;
    setupVerTrigger.setAttribute('aria-expanded', 'false');
  }
  function toggleSetupVerMenu() {
    const open = setupVerMenu.hidden;
    setupVerMenu.hidden = !open;
    setupVerTrigger.setAttribute('aria-expanded', String(open));
  }
  setupVerTrigger.addEventListener('click', (e) => { e.stopPropagation(); toggleSetupVerMenu(); });
  // 点击组件外部 / Esc 关闭；Enter/Space 展开
  document.addEventListener('click', () => closeSetupVerMenu());
  setupVerTrigger.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') { e.preventDefault(); closeSetupVerMenu(); }
    if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggleSetupVerMenu(); }
  });
  setupVerMenu.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') { e.preventDefault(); closeSetupVerMenu(); setupVerTrigger.focus(); }
  });
  // 手输版本：优先展示（联动主区），不清空下拉 label（下拉仅记录选中态）
  verManual.addEventListener('input', function () {
    const v = this.value.trim();
    if (v) { setupVerValue = v; syncSetupVerDisplay(v.startsWith('v') ? v.slice(1) : v); }
  });

  async function runSetup() {
    if (setupStarting) return;
    setupStarting = true;
    const myRun = ++setupRunId;
    const inputVer = verManual.value.trim();
    const ver = inputVer || setupVerValue;
    if (!ver) {
      setupStarting = false;
      showSetupError('无法开始安装', '版本列表为空，或未输入版本号。请检查网络连接或更换 Registry 源后重试。');
      return;
    }
    if (inputVer) {
      try {
        const exists = await invoke('version_exists_cmd', { ver: inputVer });
        if (!exists) {
          setupStarting = false;
          showSetupError('版本不存在', '版本 ' + inputVer + ' 不存在，请检查后重试。');
          return;
        }
      } catch (e) {
        setupStarting = false;
        showSetupError('版本校验失败', (typeof e === 'string' ? e : (e && e.message) || String(e)));
        return;
      }
    }
    const registry = setupReg.value.trim() || 'https://registry.npmmirror.com';
    // 预检期锁定 + 给反馈：上次安装/取消可能还在后端收尾（SETUP_BUSY 仍 true）——
    // 先等它复位再真正发起，否则新 run 会撞「已有一个安装正在进行中」，
    // 且旧 run 的 catch 会串台覆盖新 run 的 UI（「取消后再装不显示进度」根因）。
    btnSetupInstall.disabled = true;
    setupStage.textContent = '正在等待上次安装收尾…';
    for (let i = 0; i < 20; i++) {
      try {
        const st = await invoke('setup_state_cmd');
        if (!st.installing) break;
      } catch (e) { break; }
      await new Promise((r) => setTimeout(r, 500));
    }
    setupStarting = false; // 进入正常安装（此后由按钮禁用保护，不会重入）
    setupCancelled = false;
    setSetupPhase('stage'); // 从错误视图重试进入时，确保回到进行中视图（幂等）
    // 每次安装重置进度状态（fetch 计数/去重/元信息），取消重装不残留旧值
    setupFetchCount = 0;
    setupLastProgress = null;
    setupMeta.textContent = '首次安装需下载约 300MB 依赖包，通常需要几分钟，请耐心等待（可随时取消）';
    // 进入进行中态：进度条 + 取消按钮出现，安装按钮锁定
    setupProgress.hidden = false;
    btnSetupInstall.disabled = true;
    btnSetupCancel.hidden = false;
    btnSetupCancel.disabled = false;
    btnSetupCancel.textContent = '取消安装';
    setupAdv.open = false;
    setupStage.textContent = '正在连接 registry…';
    startSetupProgressPolling();
    try {
      await invoke('setup_dsh_cmd', { ver, registry });
      // 成功：后端 boot 已在后台拉起 dsh（冷启动约 5-30s）。**不依赖就绪事件**
      //（本环境实测事件会丢失）——装完后让 startSetupProgressPolling 继续跑，
      // 它轮询 dsh_url 就绪 → 收起引导切回工作台（占位保持到 workbench:ready）。
      if (!setupCancelled) {
        markSetupInstalled(ver); // 完成视图 + 进入工作台按钮；就绪由轮询自动收起
      }
    } catch (e) {
      // 若已有更新的 run（用户重新发起），本 run 的取消/失败残留不得覆盖新 UI
      if (myRun !== setupRunId) return;
      clearInterval(setupProgressTimer);
      const msg = (e && e.message) || String(e);
      if (setupCancelled) {
        // 用户主动取消：回到初始引导页（可重新安装），不显示错误态
        resetSetupInitial();
      } else if (msg.includes('已有一个安装正在进行中')) {
        // 并发保护误报：安装其实仍在进行（上次取消/安装未收尾、BUSY 仍 true）。
        // 不是失败——回到进行中态继续等真正结束（轮询已重启）；
        // 若它最终只是取消收尾（installing 变 false 且无 URL），轮询会回初始页。
        setSetupPhase('stage');
        setupStage.textContent = '安装正在进行中…';
        waitBusyReset = true;
        startSetupProgressPolling();
      } else {
        showSetupError('安装失败', msg);
      }
    }
  }

  // 进度轮询（主通道）：dsh:setup-progress 事件在部分环境不可靠
  // （T.event.listen 曾实测挂起），1s 轮询 setup_state_cmd 兜底。
  // 主通道同时取工作台 URL（dsh:url 事件实测丢失——确定性进入）与
  // 进度文本（去重走 applySetupProgress 内部 setupLastProgress）。
  function startSetupProgressPolling() {
    clearInterval(setupProgressTimer); // 幂等：先清旧 timer，防旧 run 残留泄漏/误杀新 timer
    setupProgressTimer = setInterval(async () => {
      if (!setupActive || setupCancelled) { clearInterval(setupProgressTimer); return; }
      try {
        const st = await invoke('setup_state_cmd');
        if (st.dsh_url) {
          waitBusyReset = false;
          clearInterval(setupProgressTimer);
          // 工作台由 Rust 侧创建/导航（dsh_url 出现即已 ensure_ready）；
          // 壳页只收起引导，占位保持到 workbench:ready。
          hideSetupView();
          invoke('show_workbench_cmd').catch(() => {});
          if (!workbenchReady) showPlaceholder();
          return;
        }
        // 撞 BUSY 后（waitBusyReset）：后端收尾结束（installing=false 且无 URL，
        // 如上次取消）→ 回到初始引导页，避免永久卡「安装正在进行中…」
        if (waitBusyReset && !st.installing && !st.dsh_url) {
          waitBusyReset = false;
          clearInterval(setupProgressTimer);
          resetSetupInitial();
          return;
        }
        if (st.progress) applySetupProgress(st.progress);
      } catch (e) { /* 轮询失败忽略，事件通道兜底 */ }
    }, 1000);
  }

  // 回到初始引导页：清进度/错误/取消态，恢复「安装」按钮（取消后可重新安装）
  function resetSetupInitial() {
    setupCancelled = false;
    waitBusyReset = false;
    clearInterval(setupProgressTimer); // 幂等：所有回初始的路径都停轮询，防空转泄漏
    setupProgress.hidden = true;
    setSetupPhase('stage');            // 隐藏错误/完成视图，显示 stage 区
    setupStage.textContent = '准备安装 dsh 运行时';
    setupMeta.textContent = '首次安装需下载约 300MB 依赖包，通常需要几分钟，请耐心等待；可展开高级选项选择版本与 Registry 源';
    btnSetupInstall.disabled = false;  // 安装按钮恢复可点
    btnSetupCancel.hidden = true;      // 取消按钮收回
    btnSetupCancel.disabled = false;
    btnSetupCancel.textContent = '取消安装';
    setupAdv.open = false;
    setupFetchCount = 0;
    setupLastProgress = null;
  }

  // 安装完成 → 「dsh 已就绪」完成视图（进入工作台按钮）；dsh_url 就绪后由
  // startSetupProgressPolling 自动收起引导——「进入工作台」是未就绪时的手动出口。
  function markSetupInstalled(ver) {
    setupDoneVer.textContent = 'v' + ver;
    setSetupPhase('done');
  }
  btnEnterWorkbench.addEventListener('click', () => {
    hideSetupView();
    invoke('show_workbench_cmd').catch(() => {});
    // 工作台若迟迟不就绪（dsh boot 失败等）：30s 后回到引导错误视图可重试，
    // 避免永久停在启动占位（旧「放弃等待」出口的等价恢复路径）
    (async () => {
      for (let i = 0; i < 30; i++) {
        await new Promise((r) => setTimeout(r, 1000));
        if (setupActive) return; // 期间用户已重新进入引导，放弃本轮
        try {
          if (await invoke('workbench_ready_cmd')) return;
        } catch (e) { /* 单次失败忽略 */ }
      }
      if (!setupActive) {
        showSetupError('工作台未能启动', 'dsh 已安装，但工作台长时间未就绪。可点「重试」再试，或关闭应用后重新打开。');
      }
    })();
  });

  // 高级区：Registry 源右侧「刷新版本」→ 按当前输入值重新获取可用版本
  btnRefreshVer.addEventListener('click', async () => {
    btnRefreshVer.disabled = true;
    btnRefreshVer.textContent = '刷新中…';
    await loadSetupVersions(setupReg.value.trim() || null);
    btnRefreshVer.disabled = false;
    btnRefreshVer.textContent = '刷新版本';
  });

  btnSetupInstall.addEventListener('click', () => runSetup());
  btnSetupRetry.addEventListener('click', async () => {
    // 先尝试恢复工作台（适用于「安装完成但启动失败」：dsh 可能已就绪，
    // 仅就绪信号丢失）；就绪即切回工作台，否则走重新安装。
    for (let i = 0; i < 3; i++) {
      try {
        if (await invoke('workbench_ready_cmd')) {
          hideSetupView();
          invoke('show_workbench_cmd').catch(() => {});
          return;
        }
      } catch (e) { break; }
      await new Promise((r) => setTimeout(r, 800));
    }
    // 上次安装/取消若未收尾（后端 BUSY 仍 true），先等它复位再装——
    // 否则会撞「已有一个安装正在进行中」（那只是并发保护，不是失败）。
    for (let i = 0; i < 20; i++) {
      try {
        const st = await invoke('setup_state_cmd');
        if (!st.installing) break;
      } catch (e) { break; }
      await new Promise((r) => setTimeout(r, 500));
    }
    runSetup();
  });
  setupReg.addEventListener('keydown', (e) => { if (e.key === 'Enter') runSetup(); });

  btnSetupCancel.addEventListener('click', () => {
    // 撞 BUSY 后（waitBusyReset）其实没有"真安装"在跑（是上次取消的收尾）：
    // 点取消没有可取消对象，直接回初始页，避免永久卡「正在取消安装…」
    if (waitBusyReset) { resetSetupInitial(); return; }
    setupCancelled = true;
    btnSetupCancel.disabled = true;
    btnSetupCancel.textContent = '正在取消…';
    setupStage.textContent = '正在取消安装…';
    invoke('setup_cancel_cmd').catch(() => {});
  });

  // 进度：阶段文本（setupStage）+ npm fetch 行计数（setupMeta 显示"已下载 N 个
  // 依赖包"，不刷屏）；进度条为不确定态流动动画。
  function applySetupProgress(text) {
    if (!text) return;
    if (text === setupLastProgress) return; // 双通道同一行只处理一次
    setupLastProgress = text;
    // pnpm --reporter=append-only 的进度行：只显示解析/复用/下载计数，不刷屏
    const pm = text.match(/Progress: resolved (\d+), reused (\d+), downloaded (\d+)/);
    if (pm) {
      setupStage.textContent = '正在安装依赖…';
      setupMeta.textContent = '已解析 ' + pm[1] + ' 个依赖（复用缓存 ' + pm[2] + '，下载 ' + pm[3] + '）';
      return;
    }
    // npm --loglevel=info 的 fetch 行形如 "npm http fetch GET 200 <url> <ms>"：
    // 只计数，不逐行刷 stage（否则满屏 url）；同时把阶段从「正在连接 registry」
    // 推进到「正在下载依赖包」（否则 stage 滞留旧文案与 meta 计数矛盾）。
    if (/^npm (http )?fetch/.test(text)) {
      setupFetchCount += 1;
      setupStage.textContent = '正在下载依赖包…';
      setupMeta.textContent = '已获取 ' + setupFetchCount + ' 个依赖包（约 300MB）';
      return;
    }
    setupStage.textContent = text;
  }
  T.event.listen('dsh:setup-progress', (e) => {
    if (setupActive) applySetupProgress(String(e.payload || ''));
  }).catch(() => {});

  // 辅助触发（防竞态兜底）：boot 里未装闭包会发 dsh:need-setup；webview 挂
  // 监听前发出也不怕——初始化 setup_state_cmd 探测是主通道。
  T.event.listen('dsh:need-setup', () => {
    if (!workbenchReady && !setupActive) showSetupView();
  }).catch(() => {});

  // ========================================================================
  // dsh 版本管理（抽屉 dsh 分段）
  // ========================================================================
  const dshCurrentEl = $('dshCurrent');
  const dshStatusEl = $('dshStatus');
  const dshUpdateBanner = $('dshUpdateBanner');
  const dshLatestEl = $('dshLatest');
  const btnUpdateDshEl = $('btnUpdateDsh');
  const btnCheckDshEl = $('btnCheckDsh');
  const verInputEl = $('verInput');
  const btnInstallVerEl = $('btnInstallVer');
  const btnRegistryEl = $('btnRegistry');
  let dshLatestVer = null; // 后端查询到的最新版本（与元素 id dshLatest 区分）
  let currentRegistry = 'https://registry.npmmirror.com'; // 最近一次保存/读取的 Registry 源

  function setDshStatus(text, kind) {
    dshStatusEl.textContent = text;
    dshStatusEl.className = 'status ' + (kind || '');
  }

  async function refreshDsh() {
    // 切到该分段立即给反馈：先显示 loading，数据到达后渲染（网络慢时不白屏）
    setDshStatus('正在加载…', 'run');
    const tb = $('versionRows');
    tb.innerHTML = '<tr><td colspan="3"><div class="empty">正在获取版本列表…</div></td></tr>';
    try {
      const st = await invoke('get_dsh_state');
      // st: { current, latest, versions, installing, installed }
      const current = st.current || '未安装';
      dshLatestVer = st.latest || null;
      dshCurrentEl.textContent = current;

      const hasUpdate = !!dshLatestVer && dshLatestVer !== current && current !== '未安装';
      dshUpdateBanner.hidden = !hasUpdate;
      if (hasUpdate) dshLatestEl.textContent = 'v' + dshLatestVer;
      btnUpdateDshEl.disabled = !!st.installing;
      btnCheckDshEl.disabled = !!st.installing;
      btnCheckDshEl.textContent = st.installing ? '安装中…' : '检查更新';

      renderVersions(st.versions || [], current, dshLatestVer, !!st.installing, st.installed || []);

      if (st.installing) {
        setDshStatus('正在安装新版本…安装完成后工作台自动重启', 'run');
      } else if (hasUpdate) {
        setDshStatus('发现新版本 v' + dshLatestVer + '，可更新', 'acc');
      } else if (!dshLatestVer) {
        // LATEST_DSH 为启动时一次查询的缓存；空 = 离线或检查失败，如实提示
        setDshStatus('暂无法确认最新版本（离线或启动时检查失败）', 'warn');
      } else {
        setDshStatus('已是最新版本', 'ok');
      }
    } catch (e) {
      setDshStatus('读取 dsh 状态失败：' + (e.message || e), 'err');
    }
  }

  function cmpVer(a, b) {
    const A = a.split('.').map(Number), B = b.split('.').map(Number);
    for (let i = 0; i < 3; i++) if ((A[i] || 0) !== (B[i] || 0)) return (A[i] || 0) - (B[i] || 0);
    return 0;
  }
  // 回滚目标 = 低于当前版本的已安装最高版本
  function rollbackTarget(installed, current) {
    const below = installed.filter((v) => cmpVer(v, current) < 0).sort(cmpVer);
    return below.length ? below[below.length - 1] : null;
  }

  // 版本表：每行「安装/切换/回滚」按钮点击先弹窗二次确认（文案按 op 区分），
  // 确认后执行 applyVersion（任务中心 + 进度轮询 + 状态行）。op:
  // 'install' | 'switch' | 'rollback' | 'update'
  function renderVersions(versions, current, latest, installing, installed = []) {
    const tb = $('versionRows');
    tb.innerHTML = '';
    if (!versions.length) {
      const tr = document.createElement('tr');
      tr.className = 'empty-row';
      tr.innerHTML = '<td colspan="3"><div class="empty">未获取到可用版本（检查网络或 Registry 源）</div></td>';
      tb.appendChild(tr);
      return;
    }
    // 兜底截取最近 10 个（后端已收敛，这里防御性再截一次）
    const slice = versions.slice(0, 10);
    const rb = rollbackTarget(installed, current);
    slice.forEach((v) => {
      const tr = document.createElement('tr');
      const isCur = v === current;
      const isInst = installed.includes(v);

      let st = '';
      if (v === latest) st += '<span class="badge badge-latest">最新</span> ';
      if (isCur) st += '<span class="badge badge-current">当前</span> ';
      if (isInst && !isCur) st += '<span class="badge badge-installed">已安装</span>';
      if (!st) st = '<span class="v-none">未安装</span>';
      // 版本号来自 registry（用户可配置源，不可信）——用 textContent 而非 innerHTML
      const tdVer = document.createElement('td');
      tdVer.className = 'v-ver mono';
      tdVer.textContent = 'v' + v;
      const tdSt = document.createElement('td');
      tdSt.innerHTML = st; // st 全部由上文内部徽标串构造，无外部数据
      tr.appendChild(tdVer);
      tr.appendChild(tdSt);

      const ops = document.createElement('div');
      ops.className = 'ops';
      if (isCur) {
        const b = document.createElement('button');
        b.className = 'btn ghost sm'; b.disabled = true; b.textContent = '当前版本';
        ops.appendChild(b);
      } else {
        const op = isInst ? (v === rb ? 'rollback' : 'switch') : 'install';
        const b = document.createElement('button');
        b.className = 'btn ghost sm';
        b.textContent = op === 'rollback' ? '回滚' : op === 'switch' ? '切换' : '安装';
        b.disabled = installing; // 后端安装中：全表操作锁定
        b.addEventListener('click', () => {
          const OP_MSG = {
            install: '将下载并安装 dsh v' + v + '，安装完成后工作台自动重启。',
            switch: '已安装 dsh v' + v + '，切换后工作台自动重启。',
            rollback: '将回滚到最近一个已安装旧版本 v' + v + '，回滚后工作台自动重启。',
          };
          openModal({
            title: op === 'rollback' ? '回滚到 v' + v : op === 'switch' ? '切换到 v' + v : '安装 dsh v' + v,
            message: OP_MSG[op],
            okLabel: op === 'rollback' ? '回滚' : op === 'switch' ? '切换' : '安装',
            danger: false,
            onAccept: () => applyVersion(v, b, op),
          });
        });
        ops.appendChild(b);
      }
      const td = document.createElement('td');
      td.className = 'ta-r'; td.appendChild(ops);
      tr.appendChild(td);
      tb.appendChild(tr);
    });
  }

  // 版本操作统一入口（替代旧 updateDsh 直调）：确认弹窗 → 执行 → 任务中心 +
  // setup_state_cmd 进度轮询 → 完成 Toast / 失败 Toast。首次安装（引导浮层
  // 显示中）时同步引导进度态（旧 updateDsh syncSetup 语义）。
  async function applyVersion(ver, btn, op) {
    const opCn = op === 'rollback' ? '回滚' : op === 'switch' ? '切换' : op === 'update' ? '更新' : '安装';
    const doing = op === 'rollback' ? '回滚中…' : op === 'switch' ? '切换中…' : op === 'update' ? '更新中…' : '安装中…';
    if (btn) { btn.disabled = true; btn.textContent = doing; }
    const tid = 'dsh-' + ver;
    taskStart(tid, opCn + ' dsh v' + ver);
    setDshStatus('正在' + opCn + ' dsh v' + ver + '…', 'run');
    const syncSetup = !workbenchReady && !setupOverlay.hidden;
    if (syncSetup) {
      setupActive = true;
      setSetupPhase('stage');
      setupStage.textContent = '正在安装 dsh v' + ver + '…';
      setupMeta.textContent = '首次安装需下载约 300MB 依赖包，通常需要几分钟，请耐心等待（可随时取消）';
      setupProgress.hidden = false;
      btnSetupInstall.disabled = true; // 锁定引导页安装按钮，防与抽屉安装并发（runSetup 同款保护）
      btnSetupCancel.hidden = false;
      btnSetupCancel.disabled = false;
      btnSetupCancel.textContent = '取消安装';
      setupCancelled = false;
      setupFetchCount = 0;
      setupLastProgress = null;
      startSetupProgressPolling();
    }
    btnUpdateDshEl.disabled = true;
    btnCheckDshEl.disabled = true;
    btnCheckDshEl.textContent = doing;
    $('versionRows').querySelectorAll('button').forEach((b) => { b.disabled = true; });
    // 进度显示（复用 setup_state_cmd 轮询通道）：任务中心 + 状态行双反馈
    let lastProgress = null;
    const progressTimer = setInterval(async () => {
      try {
        const st = await invoke('setup_state_cmd');
        if (st.progress && st.progress !== lastProgress) {
          lastProgress = st.progress;
          taskUpdate(tid, st.progress);
          setDshStatus(st.progress, 'run');
        }
      } catch (e) { /* 轮询失败忽略 */ }
    }, 1000);
    try {
      await invoke('update_dsh_cmd', { ver });
      clearInterval(progressTimer);
      // 后端安装成功 → dsh 已重启，工作台自动重导航（Rust workbench.ensure_ready）。
      if (syncSetup) {
        // 首次安装同步过引导：完成视图（dsh_url 就绪由引导轮询检测 → 自动收起）
        if (setupCancelled) {
          clearInterval(setupProgressTimer);
          resetSetupInitial();
          setDshStatus('已取消安装', '');
          taskFinish(tid, 'err', '已取消');
          return;
        }
        markSetupInstalled(ver);
        setDshStatus('工作台正在启动，请稍候…', 'ok');
        taskFinish(tid, 'ok', 'dsh v' + ver + ' 已就绪');
        toast('dsh v' + ver + ' 已就绪', 'ok');
      } else {
        // 非 syncSetup（工作台已在使用）：装完主动轮询 get_dsh_url（restart_dsh
        // 已清 DSH_URL，新 dsh 写入后命中）确认新进程就绪——新 URL 出现即说明
        // 工作台正在重新加载，重置就绪态让占位层盖住旧内容，等 workbench:ready 再撤。
        // 窗口会被 restart_dsh 隐藏但不销毁，此轮询在隐藏窗口内继续运行。
        setDshStatus('工作台正在启动，请稍候…', 'ok');
        taskFinish(tid, 'ok', 'dsh v' + ver + ' 已就绪');
        toast('dsh v' + ver + ' 已就绪', 'ok');
        (async () => {
          for (let i = 0; i < 38; i++) {
            try {
              const url = await invoke('get_dsh_url');
              if (url) {
                workbenchReady = false;
                showPlaceholder();
                return;
              }
            } catch (e) { /* 单次失败忽略 */ }
            await new Promise((r) => setTimeout(r, 800));
          }
          // 轮询耗尽（≈30s）仍无 url：提示可重试，不无限等（窗口隐藏，回到抽屉可见）
          setDshStatus('工作台启动超时，可点击「更新到最新」重试', 'err');
        })();
      }
    } catch (e) {
      clearInterval(progressTimer);
      // 用户主动取消（syncSetup 时在引导页点了「取消安装」）：与 runSetup 的 catch
      // 一致——回初始页、不显示「安装失败」（取消不是失败）。
      if (syncSetup && setupCancelled) {
        clearInterval(setupProgressTimer);
        resetSetupInitial();
        setDshStatus('已取消安装', '');
        taskFinish(tid, 'err', '已取消');
        return;
      }
      // 首次安装同步过引导时，失败要停引导进度轮询并恢复引导页，否则
      // setupProgressTimer 永久空转、引导浮层卡"安装中"。
      if (syncSetup) { clearInterval(setupProgressTimer); resetSetupInitial(); }
      const msg = (e && e.message) || String(e);
      setDshStatus('安装失败：' + msg, 'err');
      taskFinish(tid, 'err', msg);
      toast(msg, 'err');
    } finally {
      refreshDsh(); // 复位 installing 状态并重渲染列表
    }
    if (btn) { btn.disabled = false; btn.textContent = opCn; }
  }

  function checkDsh() { refreshDsh(); }
  btnCheckDshEl.addEventListener('click', checkDsh);
  btnUpdateDshEl.addEventListener('click', () => {
    if (!dshLatestVer) return;
    const v = dshLatestVer;
    openModal({
      title: '更新到最新',
      message: '将把 dsh 运行时更新到 v' + v + '，更新过程中工作台会重启。',
      okLabel: '更新', danger: false,
      onAccept: () => applyVersion(v, btnUpdateDshEl, 'update'),
    });
  });

  // 输入版本号安装：先校验版本存在（避免不存在的版本白白触发大下载），再弹窗确认
  btnInstallVerEl.addEventListener('click', async () => {
    const v = verInputEl.value.trim();
    if (!v) { setDshStatus('请输入版本号', 'err'); return; }
    btnInstallVerEl.disabled = true;
    try {
      const exists = await invoke('version_exists_cmd', { ver: v });
      if (!exists) { setDshStatus('版本 ' + v + ' 不存在', 'err'); return; }
      openModal({
        title: '安装 dsh v' + v,
        message: '将下载并安装 dsh v' + v + '，安装完成后工作台自动重启。',
        okLabel: '安装', danger: false,
        onAccept: () => applyVersion(v, null, 'install'),
      });
    } catch (e) {
      setDshStatus('版本校验失败：' + (e.message || e), 'err');
    } finally {
      btnInstallVerEl.disabled = false;
    }
  });
  verInputEl.addEventListener('keydown', (e) => { if (e.key === 'Enter') btnInstallVerEl.click(); });

  // Registry 源设置（弹窗）：按钮打开，预设当前值；保存走 save_registry_cmd + Toast
  async function openRegistryModal() {
    $('registryModal').hidden = false;
    const input = $('registryInput');
    try {
      const st = await invoke('get_shell_state');
      if (st && st.registry) { currentRegistry = st.registry; input.value = st.registry; }
    } catch (e) { /* 读取失败：保留当前值 */ }
    input.focus();
    input.select();
  }
  function closeRegistryModal() { $('registryModal').hidden = true; }
  async function saveRegistry() {
    const val = $('registryInput').value.trim();
    if (!val) { toast('Registry 源不能为空', 'err'); return; }
    const btn = $('registrySave');
    btn.disabled = true;
    btn.textContent = '保存中…';
    try {
      await invoke('save_registry_cmd', { registry: val });
      // 回显规范化后的地址（如去掉尾部斜杠）
      try {
        const st = await invoke('get_shell_state');
        if (st && st.registry) currentRegistry = st.registry;
      } catch (e) { currentRegistry = val; }
      closeRegistryModal();
      toast('Registry 源已保存', 'ok');
    } catch (e) {
      toast('保存失败：' + (e.message || e), 'err');
    } finally {
      btn.disabled = false;
      btn.textContent = '保存';
    }
  }
  btnRegistryEl.addEventListener('click', openRegistryModal);
  $('registryClose').addEventListener('click', closeRegistryModal);
  $('registryCancel').addEventListener('click', closeRegistryModal);
  $('registryModal').addEventListener('click', (e) => { if (e.target === $('registryModal')) closeRegistryModal(); });
  $('registryInput').addEventListener('keydown', (e) => { if (e.key === 'Enter') $('registrySave').click(); });
  $('registrySave').addEventListener('click', saveRegistry);

  // ========================================================================
  // 插件管理（抽屉 插件 分段）
  // ========================================================================
  const pkgInputEl = $('pkgInput');
  const btnInstallPkgEl = $('btnInstallPkg');
  const btnRemovePkgEl = $('btnRemovePkg');
  const pluginListEl = $('pluginList');
  const pluginCountEl = $('pluginCount');
  const pluginEmptyEl = $('pluginEmpty');
  const pluginLogEl = $('pluginLog');
  const pluginLogEmptyEl = $('pluginLogEmpty');
  let pluginBusy = false;

  function nowT() {
    const d = new Date(), p = (n) => String(n).padStart(2, '0');
    return p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
  }
  // 惰性显示：首行日志到达才切出日志框（替代旧的常驻状态行）
  function logLine(text, cls) {
    pluginLogEmptyEl.hidden = true;
    pluginLogEl.hidden = false;
    const line = document.createElement('div');
    const t = document.createElement('span');
    t.className = 'l-time';
    t.textContent = '[' + nowT() + '] ';
    const m = document.createElement('span');
    if (cls) m.className = cls;
    m.textContent = text;
    line.appendChild(t);
    line.appendChild(m);
    pluginLogEl.appendChild(line);
    pluginLogEl.scrollTop = pluginLogEl.scrollHeight;
  }
  function clearPluginLog() {
    pluginLogEl.textContent = '';
    pluginLogEl.hidden = true;
    pluginLogEmptyEl.hidden = false;
  }
  function setPluginBusy(busy) {
    pluginBusy = busy;
    btnInstallPkgEl.disabled = busy;
    btnRemovePkgEl.disabled = busy;
    pkgInputEl.disabled = busy;
  }

  async function refreshPlugins() {
    try {
      const list = await invoke('plugin_list_cmd');
      const arr = Array.isArray(list) ? list : [];
      pluginListEl.innerHTML = '';
      pluginCountEl.textContent = arr.length + ' 个插件';
      pluginEmptyEl.hidden = arr.length > 0;
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
        if (p.installed) {
          const btn = document.createElement('button');
          btn.className = 'btn danger-ghost sm';
          btn.textContent = '卸载';
          // 卸载二次确认改 modal（删除旧「行内 3 秒确认」按钮逻辑）
          btn.addEventListener('click', () => {
            openModal({
              title: '卸载插件',
              message: '将卸载 ' + p.name + '，操作完成后工作台自动重启。',
              okLabel: '卸载', danger: true,
              onAccept: () => doRemovePlugin(p.name),
            });
          });
          row.appendChild(btn);
        }
        pluginListEl.appendChild(row);
      });
    } catch (e) {
      pluginCountEl.textContent = '读取失败';
      logLine('读取插件列表失败：' + (e.message || e), 'l-warn');
    }
  }

  // 卸载执行（modal 确认后调用，列表行与输入框旁按钮共用）；执行期间锁全局安装入口
  async function doRemovePlugin(pkg) {
    setPluginBusy(true);
    const tid = 'pkg-' + pkg;
    taskStart(tid, '卸载插件');
    taskUpdate(tid, pkg);
    logLine('正在卸载 ' + pkg + '…');
    try {
      const text = await invoke('plugin_op', { op: 'remove', pkg });
      if (text) logLine(text);
      logLine(pkg + ' 已卸载，工作台正在重启…', 'l-ok');
      taskFinish(tid, 'ok', pkg + ' 已卸载');
      toast(pkg + ' 已卸载', 'ok');
    } catch (e) {
      const msg = typeof e === 'string' ? e : (e && e.message) || String(e);
      logLine('卸载失败：' + msg, 'l-warn');
      taskFinish(tid, 'err', msg);
      toast('卸载失败', 'err');
    } finally {
      setPluginBusy(false);
      refreshPlugins();
    }
  }

  async function runPlugin() {
    const name = pkgInputEl.value.trim();
    if (!name) { toast('请输入包名', 'err'); return; }
    setPluginBusy(true);
    const tid = 'pkg-' + name;
    taskStart(tid, '安装插件');
    taskUpdate(tid, name);
    logLine('正在解析 ' + name + '…');
    try {
      const text = await invoke('plugin_op', { op: 'add', pkg: name });
      if (text) logLine(text);
      logLine(name + ' 已安装，工作台正在重启…', 'l-ok');
      taskFinish(tid, 'ok', name + ' 已安装');
      toast('插件已安装', 'ok');
      pkgInputEl.value = '';
    } catch (e) {
      const msg = typeof e === 'string' ? e : (e && e.message) || String(e);
      logLine('安装失败：' + msg, 'l-warn');
      taskFinish(tid, 'err', msg);
      toast('安装失败', 'err');
    } finally {
      setPluginBusy(false);
      refreshPlugins();
    }
  }
  btnInstallPkgEl.addEventListener('click', () => runPlugin());
  btnRemovePkgEl.addEventListener('click', () => {
    const name = pkgInputEl.value.trim();
    if (!name) { toast('请输入包名', 'err'); return; }
    openModal({
      title: '卸载插件',
      message: '将卸载 ' + name + '，操作完成后工作台自动重启。',
      okLabel: '卸载', danger: true,
      onAccept: () => doRemovePlugin(name),
    });
  });
  pkgInputEl.addEventListener('keydown', (e) => { if (e.key === 'Enter') runPlugin(); });
  $('btnClearLog').addEventListener('click', clearPluginLog);

  // 实时输出：插件构建脚本的行级推送（后端已自动重启工作台）
  T.event.listen('dsh:plugin-output', (e) => {
    logLine(String(e.payload || ''));
  }).catch(() => {});

  // ========================================================================
  // 关于（抽屉 关于 分段：App 更新 / 两档卸载）
  // ========================================================================
  const appVersionEl = $('appVersion');
  const appStatusEl = $('appStatus');
  const btnCheckAppEl = $('btnCheckApp');
  const btnDownloadAppEl = $('btnDownloadApp');
  let appLatest = null;

  function setAppStatus(text, kind) {
    appStatusEl.textContent = text;
    appStatusEl.className = 'status ' + (kind || '');
  }

  async function refreshApp() {
    try {
      const st = await invoke('get_shell_state');
      appVersionEl.textContent = 'v' + st.app_version;
    } catch (e) {
      appVersionEl.textContent = '未知';
    }
  }

  async function checkApp() {
    const btn = btnCheckAppEl;
    btn.disabled = true;
    btn.textContent = '检查中…';
    setAppStatus('正在检查更新…');
    try {
      const v = await invoke('check_app_update_cmd');
      if (v) {
        appLatest = v;
        btnDownloadAppEl.disabled = false;
        btnDownloadAppEl.textContent = '下载并安装 v' + v;
        setAppStatus('发现新版本 v' + v, 'acc');
      } else {
        appLatest = null;
        btnDownloadAppEl.disabled = true;
        btnDownloadAppEl.textContent = '下载并安装更新';
        setAppStatus('已是最新版本', 'ok');
      }
    } catch (e) {
      setAppStatus('检查更新失败：' + (e.message || e), 'err');
    } finally {
      btn.disabled = false;
      btn.textContent = '检查更新';
    }
  }
  btnCheckAppEl.addEventListener('click', checkApp);
  btnDownloadAppEl.addEventListener('click', async () => {
    btnDownloadAppEl.disabled = true;
    setAppStatus('正在下载并安装更新…安装完成后应用将自动重启');
    const tid = 'app-update';
    taskStart(tid, '下载应用更新');
    taskUpdate(tid, '正在下载并安装…');
    try {
      await invoke('app_update_cmd');
      // 成功即退出当前实例（安装器/新版负责启动）；任务态到此为止
      taskFinish(tid, 'ok', '更新包已就绪，重启后生效');
    } catch (e) {
      taskFinish(tid, 'err', (e && e.message) || String(e));
      setAppStatus('更新失败：' + ((e && e.message) || e), 'err');
      btnDownloadAppEl.disabled = !appLatest;
    }
  });
  $('btnOpenReleases').addEventListener('click', () => {
    invoke('open_browser_cmd').catch((e) => setAppStatus('打开下载页失败：' + (e.message || e), 'err'));
  });
  $('repoLink').addEventListener('click', (e) => {
    e.preventDefault();
    invoke('open_repo_cmd').catch((e) => setAppStatus('打开项目主页失败：' + (e.message || e), 'err'));
  });

  // 卸载（两档）：面板默认收起，点「卸载应用…」展开两行卡片；二次确认由后端
  // confirm_uninstall_cmd 的系统确认 modal 承担（前端不换 modal）。
  const btnUninstallKeep = $('btnUninstallKeep');
  const btnUninstallWipe = $('btnUninstallWipe');
  const uninstallStatus = $('uninstallStatus');
  $('btnToggleUninstall').addEventListener('click', function () {
    const zone = $('uninstallZone');
    const open = zone.hidden;
    zone.hidden = !open;
    this.textContent = open ? '收起卸载选项' : '卸载应用…';
  });
  function runUninstall(wipe) {
    uninstallStatus.textContent = '正在卸载…';
    uninstallStatus.className = 'status';
    [btnUninstallKeep, btnUninstallWipe].forEach((b) => { b.disabled = true; });
    taskStart('uninstall', '卸载应用');
    taskUpdate('uninstall', '等待系统确认…');
    invoke('confirm_uninstall_cmd', { wipe })
      .then(() => {
        // 成功返回 = 用户取消（确认后真正卸载会退出 App，此处复位仅影响取消路径）
        [btnUninstallKeep, btnUninstallWipe].forEach((b) => { b.disabled = false; });
        uninstallStatus.textContent = '';
        uninstallStatus.className = 'status';
        taskFinish('uninstall', 'err', '已取消卸载');
      })
      .catch((e) => {
        uninstallStatus.textContent = '卸载未完成：' + (e.message || e);
        uninstallStatus.className = 'status err';
        [btnUninstallKeep, btnUninstallWipe].forEach((b) => { b.disabled = false; });
        taskFinish('uninstall', 'err', '卸载未完成');
      });
  }
  btnUninstallKeep.addEventListener('click', () => runUninstall(false));
  btnUninstallWipe.addEventListener('click', () => runUninstall(true));

  // ========================================================================
  // 初始渲染 + 引导探测
  // ========================================================================
  (async () => {
    // 引导状态探测（主通道）：current 为 None → 显示引导浮层。
    // dsh:need-setup 事件可能在 webview 挂监听前发出而丢失，仅作辅助。
    try {
      const st = await invoke('setup_state_cmd');
      if (!st.current) showSetupView();
    } catch (e) { /* 探测失败：依赖 dsh:need-setup 事件与工作台轮询兜底 */ }

    // 各分段初始数据（抽屉打开时切换分段会再刷新，启动预取保持旧行为）
    refreshDsh();
    refreshApp();
    refreshPlugins();
  })();
})();