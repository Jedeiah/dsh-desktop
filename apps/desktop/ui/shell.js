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
  // 壳页「被原生裁到顶栏」的判定阈值：工作台可见且无浮层时，Rust 把壳页 webview 裁到
  // 36pt（折叠 8pt），此时视口高度远小于此值。两个用途：toast 跳过（裁切态画不出来）、
  // 打开浮层前先解裁（见 afterUnclip）。
  const CLIP_H = 120;
  // 被裁切时先让 Rust 解除裁切（hide_workbench_cmd），再执行「显示浮层」这步：
  // 否则浮层会按 36pt 视口首帧布局（实测：打开命令面板 palH=36，且下一帧仍未解裁），
  // 解裁后才跳成整幅——观感是"啪地弹出"而非滑入。未裁切时直接同步执行，不引入延迟。
  function afterUnclip(fn) {
    if (window.innerHeight >= CLIP_H) { fn(); return; }
    invoke('hide_workbench_cmd').catch(() => {}).then(fn);
  }

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

  // ---------------- 平台相关文案：macOS 显示 ⌘，其他平台显示 Ctrl+ ----------------
  // HTML 里的快捷键键帽用 data-modkey="MODK" / "MOD1–4" 标记，运行时把 MOD 替换成
  // ⌘（mac）或 Ctrl+（Win/Linux），两种平台都能得到习惯写法（⌘K / Ctrl+K）。
  const IS_MAC = /Mac|iP(hone|ad|od)/i.test(navigator.platform || navigator.userAgent || '');
  const MODKEY = IS_MAC ? '⌘' : 'Ctrl+';
  document.querySelectorAll('[data-modkey]').forEach((el) => {
    el.textContent = (el.getAttribute('data-modkey') || '').replace(/MOD/g, MODKEY);
  });

  // 真实窗口是否「小窗」（窄 ≤900pt 或矮 ≤620pt）：由 Rust 按真实窗口尺寸判定，初值来自
  // get_shell_state.window_small，之后由主窗 Resized 事件更新。**不能改用 CSS 的
  // max-height 媒体查询**：壳页视口在「工作台可见且无浮层」时被裁到只剩顶栏（≈36px），
  // 会把这种正常状态误判成矮窗（抽屉打开那一帧闪成整幅宽度）。
  window.__onWindowSize = (small) => {
    document.body.classList.toggle('small-window', !!small);
  };

  // ---------------- 页面滚动兜底 ----------------
  // 壳页是固定布局（html/body 均 overflow:hidden），但 overflow:hidden **挡不住
  // 程序化滚动**：抽屉滑入动画期间若对离屏元素执行 focus()（或浏览器把聚焦元素
  // 滚入视野），页面会被横向滚动一个抽屉宽，顶栏随之滚出视口且不会自己回来——
  // 用户实测症状「召回后管理行不见了、也找不到展开把手」（实测 chrome.x 从 0 变
  // -560 = --drawer-w）。这里在任何滚动后归零，状态切换点也主动调一次。
  function resetPageScroll() {
    const d = document.documentElement;
    const b = document.body;
    if (d.scrollLeft || d.scrollTop) { d.scrollLeft = 0; d.scrollTop = 0; }
    if (b.scrollLeft || b.scrollTop) { b.scrollLeft = 0; b.scrollTop = 0; }
  }
  window.addEventListener('scroll', resetPageScroll, { passive: true });

  // ---------------- Toast（短反馈，2.8s 消失） ----------------
  // 清掉正在显示的 toast。壳页即将被原生裁回顶栏时（浮层收起 / 工作台归位）必须清：
  // toast 是「视口底部」定位，视口一被裁成 36px，它就会重新布局到那一条里、压在管理行上
  // （用户实测：按 Esc 回工作台，之前的提示还留在管理行上）。
  function clearToasts() {
    const box = $('toasts');
    if (box) box.textContent = '';
  }

  function toast(msg, kind) {
    // 壳页在「工作台可见且无浮层」时被原生裁到只剩顶栏（视口 36px/8px，见
    // apply_shell_clip_on_main）：此时任何 HTML 提示都只能挤在那一条里，必然盖住
    // 「工作台 / 管理」且被裁断（用户反馈），而这类动作通常已有可见结果。故直接跳过。
    if (window.innerHeight < CLIP_H) return;
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

  // 兜底看门狗：本项目 T.event.listen 曾实测丢失/挂起，而 workbench:ready 只有
  // 「事件」一条通道时，若那次事件丢了，占位层会永久盖住工作台（重装/切换 dsh 后
  // 用户只能重启应用）。故每次显示占位时启动受限轮询（最多 120s），拿到就绪即撤。
  let readyWatchdog = null;
  function startReadyWatchdog() {
    if (readyWatchdog) return;
    let n = 0;
    readyWatchdog = setInterval(async () => {
      if (workbenchReady || ++n > 120) {
        clearInterval(readyWatchdog);
        readyWatchdog = null;
        return;
      }
      try {
        if (await invoke('workbench_ready_cmd')) {
          clearInterval(readyWatchdog);
          readyWatchdog = null;
          onWorkbenchReady();
        }
      } catch (e) { /* 单次失败忽略 */ }
    }, 1000);
  }

  function showPlaceholder() {
    // 先清内联 display 再取消 hidden（hidden=false 但残留 inline display:none
    // 时仍会隐藏；反之先 hidden=false 再清 display 会出现一帧闪变）
    clearTimeout(placeholderTimer);
    startupView.style.display = '';
    startupView.hidden = false;
    startReadyWatchdog();
  }
  // 加载页信息行：App 版本 · dsh 版本 · 工作台端口（拿不到就留空，不报错）
  (async () => {
    try {
      const st = await invoke('get_shell_state');
      if (st) window.__onWindowSize(st.window_small);
      const url = await invoke('get_dsh_url');
      const port = url ? (String(url).match(/:([0-9]+)/) || [])[1] : '';
      const parts = [];
      if (st && st.app_version) parts.push('App v' + st.app_version);
      if (st && st.dsh_version && st.dsh_version !== '未知') parts.push('dsh ' + st.dsh_version);
      if (port) parts.push('工作台 :' + port);
      const el = $('startupMeta');
      if (el) el.textContent = parts.join('  ·  ');
    } catch (e) { /* 忽略 */ }
  })();

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
    // 撤占位延迟 1.2s：workbench:ready 是「工作台已移入窗口」后发出的，但原生视图
    // 把预渲染内容真正画出来还需要若干帧——撤太早（原 300ms）会出现「占位已撤、
    // 工作台还没画出来」的空档（用户反馈：加载页隐藏后先看到背景才看到 dsh）。
    placeholderTimer = setTimeout(() => hidePlaceholder(), 1200);
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

  // ---------------- 品牌：单击刷新，双击系统浏览器打开（350ms 延时消歧） ----------------
  // 判定窗口从 200ms 放宽到 350ms：人手双击间隔常见 250-400ms，原值下慢一点的
  // 双击会退化成「两次单击刷新」，表现为「双击没反应」。
  let brandTimer = null;
  $('brand').addEventListener('click', () => {
    clearTimeout(brandTimer);
    brandTimer = setTimeout(() => invoke('workbench_reload_cmd').catch(() => {}), 350);
  });
  $('brand').addEventListener('dblclick', () => {
    clearTimeout(brandTimer);
    // 不再弹 toast：动作本身（浏览器起来）就是反馈，而壳页被裁到只剩顶栏时 toast 只能
    // 压在那 36px 里、还会盖住「工作台/管理」（用户反馈）。
    invoke('open_workbench_url_cmd').catch(() => {});
  });

  // ---------------- 顶栏折叠（状态记忆；Rust 几何联动 workbench_set_collapsed_cmd） ----------------
  // 原交互：顶栏右上角「收起」按钮折叠；折叠后顶栏滑出、窗口顶部中央出现展开
  // 把手（chromeRestore）。bug 修复：折叠态工作台预留 8px 把手条（与 Rust HANDLE_H_LOGICAL、
  // 层级更高的原生工作台盖住（此前折叠后点不到展开按钮）。
  let chromeCollapsed = false; // 供命令面板把文案/行为切换成「展开导航栏」
  function setChromeCollapsed(v) {
    chromeCollapsed = !!v;
    reassertChromeDom();
    invoke('workbench_set_collapsed_cmd', { collapsed: v }).catch(() => {});
  }
  // 只按当前状态重写 DOM（类名 + 把手可访问性），不通知 Rust、不触发几何动画：
  // 用于「关到后台 → 召回」等场景重新断言，消除任何 DOM 漂移。
  function reassertChromeDom() {
    document.body.classList.toggle('chrome-collapsed', chromeCollapsed);
    $('chromeRestore').setAttribute('aria-hidden', String(!chromeCollapsed));
  }
  $('btnCollapseChrome').addEventListener('click', () => setChromeCollapsed(true));
  $('chromeRestore').addEventListener('click', () => setChromeCollapsed(false));
  // 启动总是展开顶栏（不恢复上次折叠状态）：
  // 用户实测反馈——折叠状态一旦持久化，重启应用就会看到「管理那一行整行消失」，
  // 而展开把手只是窗口顶部中央一个 52×8 的小条，不易发现，体验不可接受。
  // 折叠功能本身保留（右上角按钮 / ⌘K 命令面板的「收起 / 展开导航栏」），仅不再跨会话记忆；
  // 同时清除历史遗留标记，避免旧值继续影响。
  setChromeCollapsed(false);
  try {
    localStorage.removeItem('chromeCollapsed');
    localStorage.removeItem('tabsCollapsed');
  } catch (e) { /* 忽略 */ }

  // 「收起导航栏」防误触开关：抽屉 / 命令面板打开期间禁用折叠按钮——两者都是
// 右上角附近的浮层，用户常把折叠按钮当作浮层关闭按钮点击，导致整个顶栏消失
// （症状：管理那一行不见了、还找不到展开把手）。
  function syncCollapseGuard() {
    const overlayOpen = paletteOpen || $('drawer').classList.contains('open');
    $('btnCollapseChrome').disabled = overlayOpen;
    // 浮层打开 = 工作台让位、背景露出：显示氛围信息卡并让图标作为背景主视觉
    document.body.classList.toggle('overlay-open', overlayOpen);
  }

  // ---------------- 区域 / 抽屉 ----------------
  const REGIONS = [
    { id: 'workbench', label: '工作台', hint: '关闭浮层，回到 dsh 工作台', icon: ICON.layers },
    { id: 'dsh', label: 'dsh 版本', hint: '版本、更新与 Registry 源', icon: ICON.terminal },
    { id: 'plugins', label: '插件', hint: '安装 / 卸载插件，查看输出', icon: ICON.folder },
    { id: 'about', label: '关于', hint: 'App 版本、更新与卸载', icon: ICON.info },
  ];
  const COMMANDS = [
    { id: 'refresh', label: '刷新工作台', hint: '重新加载 dsh 工作台', icon: ICON.refresh, run: () => invoke('workbench_reload_cmd').catch(() => {}) },
    { id: 'openBrowser', label: '在浏览器打开工作台', hint: '用系统默认浏览器打开当前 dsh 地址', icon: ICON.external, run: () => { invoke('open_workbench_url_cmd').catch(() => {}); } },
    { id: 'checkDsh', label: '检查 dsh 更新', hint: '立即检查 dsh 运行时新版本', icon: ICON.terminal, run: () => { openDrawer('dsh'); checkDsh(); } },
    // 重启 dsh：结束 dsh 子进程并重新拉起（端口会变、工作台重连）。用在「终端里改过插件/配置」
    // 之后让 App 里的运行时重新加载；会中断正在运行的任务，故二次确认。
    {
      id: 'restartDsh',
      label: '重启 dsh',
      hint: '重新拉起 dsh 运行时（在终端改过插件或配置后用）',
      icon: ICON.refresh,
      run: () => openModal({
        title: '重启 dsh',
        message: '将结束当前 dsh 进程并重新启动：正在运行的任务会中断，工作台会重新加载（端口会变）。',
        okLabel: '重启', danger: false,
        onAccept: () => {
          if (appUpdating) { toast('应用正在更新，请稍候再操作', 'err'); return; }
          invoke('dsh_restart_cmd')
            .then(() => toast('dsh 已重启', 'ok'))
            .catch((e) => toast('重启失败：' + ((e && e.message) || e), 'err'));
        },
      }),
    },
    { id: 'checkApp', label: '检查应用更新', hint: '检查 DeepSeek Harness Desktop 更新', icon: ICON.info, run: () => { openDrawer('about'); checkApp(); } },
    // label/hint 用 getter：命令面板每次渲染都会取到当前折叠态对应的文案
    // （展开时显示「收起导航栏」，已收起时显示「展开导航栏」），点击即切换。
    {
      id: 'toggleChrome',
      get label() { return chromeCollapsed ? '展开导航栏' : '收起导航栏'; },
      get hint() { return chromeCollapsed ? '恢复顶栏与导航区' : '折叠顶栏以扩展工作区'; },
      icon: ICON.chevron,
      run: () => {
        // 只挡「收起」：抽屉占用工作区时折叠会让顶栏消失、抽屉顶到窗口最上沿
        // （`.chrome-collapsed .drawer { top: 0 }`），观感像"导航栏里显示了抽屉内容"（实测）；
        // 「展开」必须放行——顶部把手此时本来就点得动，命令不能比它更严（用户实测）。
        if (!chromeCollapsed && $('drawer').classList.contains('open')) {
          toast('请先收起管理抽屉', 'err');
          return;
        }
        setChromeCollapsed(!chromeCollapsed);
      },
    },
  ];
  let region = 'workbench';

  // 氛围信息块（抽屉/面板打开时露出）：工作台地址 + dsh 版本
  async function refreshAmbientInfo() {
    try {
      const st = await invoke('get_shell_state');
      const url = await invoke('get_dsh_url');
      const u = $('aiUrl');
      if (u) u.textContent = url ? String(url).replace(/\?token=[^&]*/, '') : '未就绪';
      const d = $('aiDsh');
      if (d && st && st.dsh_version) d.textContent = st.dsh_version;
    } catch (e) { /* 忽略 */ }
  }

  // 浮层显示代的令牌：afterUnclip 会把「显示」推后一个 IPC 往返，期间若用户又关了
  // 抽屉/面板，迟到的回调不得把浮层重新打开（close 也自增来作废在途回调）。
  let overlayGen = 0;

  function openDrawer(section) {
    region = section;
    refreshAmbientInfo();
    const drawer = $('drawer');
    drawer.querySelectorAll('.drawer-section').forEach((s) => s.classList.toggle('active', s.dataset.section === section));
    drawer.querySelectorAll('.segmented button').forEach((b) => {
      const on = b.dataset.section === section;
      b.classList.toggle('active', on);
      b.setAttribute('aria-selected', String(on));
    });
    const gen = ++overlayGen;
    afterUnclip(() => {
      if (gen !== overlayGen) return; // 期间已被收起/切换：放弃这次显示
      drawer.inert = false; // 关闭态整体不可聚焦（见 drawerEl.inert 注释）
      drawer.classList.add('open');
      // 抽屉占用右侧 560px：水印居中于剩余区域（theme.css 的 body.drawer-open）
      document.body.classList.add('drawer-open');
      // 抽屉打开期间禁用「收起导航栏」：折叠按钮紧邻抽屉关闭按钮、都是右上角，
      // 极易误触导致整个顶栏消失（用户多次踩坑）。折叠仍可在收起抽屉后/⌘K
      // 命令面板里进行。
      syncCollapseGuard();
      resetPageScroll();
      // 工作台是原生 webview，盖在所有 HTML 之上：抽屉打开必须显式隐藏
      if (section !== 'workbench') invoke('hide_workbench_cmd').catch(() => {});
    });
    // 切到该分段时刷新数据（安装/插件状态可能已在后台变化）
    if (section === 'dsh') refreshDsh();
    if (section === 'plugins') refreshPlugins();
    if (section === 'about') refreshApp();
  }
  // 恢复工作台：只有「没有任何浮层占用工作区」时才下发原生命令。
  // 事故背景：closeDrawer 的 260ms 延时回调原先无条件 show——若这 260ms 内又打开
  // 命令面板/抽屉，原生工作台会被移回窗口内并盖住 HTML 浮层（表现为「点了管理没
  // 反应 / 面板一闪就没」）；启动占位期间还会把尚未绘制首帧的工作台提前移入。
  function maybeShowWorkbench() {
    if (paletteOpen) return;
    if ($('drawer').classList.contains('open')) return;
    clearToasts(); // 工作台即将回归、壳页将被裁回顶栏：先清掉定位在视口底部的提示
    invoke('show_workbench_cmd').catch(() => {});
  }
  function closeDrawer(restoreWorkbench = true) {
    clearToasts(); // 抽屉一收起，视口马上会被裁回顶栏（见 clearToasts 注释）
    overlayGen++; // 作废在途的显示回调（见 openDrawer）
    const drawer = $('drawer');
    drawer.classList.remove('open');
    drawer.inert = true; // 关闭态不可聚焦：否则 Tab/输入会落进离屏抽屉
    // 焦点若还在抽屉里（如刚填完版本号），关掉后必须移出——离屏输入框会继续吞按键。
    if (drawer.contains(document.activeElement)) document.activeElement.blur();
    document.body.classList.remove('drawer-open');
    region = 'workbench';
    syncCollapseGuard();
    resetPageScroll();
    // 等抽屉收回动画（0.2s transition）播完再恢复工作台：立即恢复会让工作台
    // 突然盖住还在滑动中的抽屉，观感变成「收回没有动效、一下消失」
    if (restoreWorkbench) setTimeout(maybeShowWorkbench, 260);
  }
  // 收起所有浮层。restoreWorkbench=false 用于「关到后台」这类不能动工作台的场合
  // （此刻窗口不可见，把原生工作台移回窗口内会在 macOS 上变成孤立浮窗）。
  function closeOverlays(restoreWorkbench = true) {
    closePalette(restoreWorkbench);
    if ($('drawer').classList.contains('open')) closeDrawer(restoreWorkbench);
  }
  // 关到后台（红点 / ⌘W）时由 Rust 调用：浮层不能跨「隐藏→召回」存活。
  // 否则召回时 reveal 会无条件 show_child，原生工作台盖在所有 HTML 之上，
  // 抽屉/面板被压在下面——用户看到的是「workbench 上残留一个输入框和按钮」。
  window.__onHideToTray = () => {
    closeOverlays(false);
    reassertChromeDom();
  };
  // 主窗被召回（托盘「显示主窗口」/ Dock 点击 / 启动首显）时由 Rust 调用：
  // 先清掉浮层再让工作台归位，避免同样的"浮层被盖住/反过来盖住工作台"错配。
  window.__onMainReveal = () => {
    closeOverlays(false);
    resetPageScroll(); // 召回时归零页面滚动（否则顶栏可能停在视口外）
    reassertChromeDom(); // 折叠态跨「隐藏 → 召回」重新断言，消除顶栏/把手的 DOM 漂移
    maybeShowWorkbench();
  };
  function goRegion(id) {
    if (id === 'workbench') closeDrawer();
    else openDrawer(id);
  }
  // ---------------- 抽屉宽度可拖动（左边缘 6px 热区） ----------------
  // 宽度写进 --drawer-w：抽屉本体、氛围水印居中、窄窗判定都读它。持久化到 localStorage，
  // 越界自动夹紧（最小 360，最大 min(900, 窗口宽 − 420)，给右侧工作台留空间）。
  const DRAWER_MIN = 360, DRAWER_DEFAULT = 560, DRAWER_RESERVED = 420;
  const drawerEl = $('drawer');
  const drawerResizeEl = $('drawerResize');
  // 抽屉关闭时只靠 transform 移到屏外（没有 hidden/visibility），里面的输入框仍在
  // Tab 序里、仍能拿焦点：关掉抽屉后继续打字会落进看不见的输入框，Tab 也会跑进离屏
  // 控件（`resetPageScroll` 还会把由此产生的滚动立刻归零，观感是"焦点不见了"）。
  // inert 让关闭态的抽屉整体不可聚焦/不可点，且不影响滑入滑出过渡。
  drawerEl.inert = true;
  const drawerW = () =>
    parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--drawer-w')) || DRAWER_DEFAULT;
  function clampDrawerWidth(px) {
    const max = Math.max(DRAWER_MIN, Math.min(900, window.innerWidth - DRAWER_RESERVED));
    return Math.max(DRAWER_MIN, Math.min(max, Math.round(px)));
  }
  function setDrawerWidth(px, persist) {
    const w = clampDrawerWidth(px);
    document.documentElement.style.setProperty('--drawer-w', w + 'px');
    if (persist) { try { localStorage.setItem('drawerWidth', String(w)); } catch (e) { /* 忽略 */ } }
    return w;
  }
  // 启动恢复（旧值越界会被夹紧；窄窗下 CSS 仍会让抽屉转整幅）
  (function restoreDrawerWidth() {
    let saved = DRAWER_DEFAULT;
    try {
      const v = Number(localStorage.getItem('drawerWidth'));
      if (Number.isFinite(v) && v > 0) saved = v;
    } catch (e) { /* 忽略 */ }
    setDrawerWidth(saved, false);
  })();
  window.addEventListener('resize', () => setDrawerWidth(drawerW(), false));
  drawerResizeEl.addEventListener('pointerdown', (e) => {
    e.preventDefault();
    document.body.classList.add('drawer-resizing'); // 拖动期间全局 col-resize + 禁选中
    drawerResizeEl.setPointerCapture(e.pointerId);
    const move = (ev) => setDrawerWidth(window.innerWidth - ev.clientX, false);
    // 收尾共用一条路径：pointerup 与 pointercancel（触控/系统手势/窗口失焦打断）
    // 都要走——只监听 pointerup 时，cancel 会让 body.drawer-resizing 永久残留
    // （全局 col-resize、无法选中文字），并让下一次 pointerdown 再叠一份 move/up。
    const end = (ev, persist) => {
      drawerResizeEl.removeEventListener('pointermove', move);
      drawerResizeEl.removeEventListener('pointerup', up);
      drawerResizeEl.removeEventListener('pointercancel', cancel);
      if (drawerResizeEl.hasPointerCapture(e.pointerId)) {
        try { drawerResizeEl.releasePointerCapture(e.pointerId); } catch (err) { /* 已释放 */ }
      }
      document.body.classList.remove('drawer-resizing');
      if (persist) setDrawerWidth(window.innerWidth - ev.clientX, true); // 松手才落盘
    };
    const up = (ev) => end(ev, true);
    const cancel = (ev) => end(ev, false); // 中断不落盘：保持上次已持久化的宽度
    drawerResizeEl.addEventListener('pointermove', move);
    drawerResizeEl.addEventListener('pointerup', up);
    drawerResizeEl.addEventListener('pointercancel', cancel);
  });
  // 双击复位；键盘 ←/→（Shift 加速）、Home 复位（把手 tabindex=0）
  drawerResizeEl.addEventListener('dblclick', () => {
    try { localStorage.removeItem('drawerWidth'); } catch (e) { /* 忽略 */ }
    setDrawerWidth(DRAWER_DEFAULT, false);
  });
  drawerResizeEl.addEventListener('keydown', (e) => {
    const step = e.shiftKey ? 48 : 12;
    if (e.key === 'ArrowLeft') { e.preventDefault(); setDrawerWidth(drawerW() + step, true); }
    else if (e.key === 'ArrowRight') { e.preventDefault(); setDrawerWidth(drawerW() - step, true); }
    else if (e.key === 'Home') { e.preventDefault(); setDrawerWidth(DRAWER_DEFAULT, true); }
  });

  $('drawerClose').addEventListener('click', closeDrawer);
  $('drawer').querySelectorAll('.segmented button').forEach((b) => {
    b.addEventListener('click', () => openDrawer(b.dataset.section));
  });

  // ---------------- 命令面板 ----------------
  let paletteOpen = false, paletteSel = 0, paletteItems = [];
  function openPalette() {
    paletteOpen = true;
    resetPageScroll();
    // 面板背后就是氛围层的信息卡（工作台地址 / dsh 版本）：openDrawer 一直会刷新，
    // 面板这条路径漏了——先开面板再开抽屉之前，卡片显示的是「工作台 — / dsh —」
    // （真机截图可见）。两条浮层入口都刷新一次。
    refreshAmbientInfo();
    const gen = ++overlayGen;
    // 先解裁再显示（同抽屉，见 afterUnclip）：面板是 fixed inset:0，被裁到 36pt 时
    // 首帧只有那条细高度，解裁后才跳成整幅。
    afterUnclip(() => {
      if (gen !== overlayGen || !paletteOpen) return;
      // 已显示（极端快速连按）时不再重置输入与选中项：期间用户可能已用 ↑↓ 选过项
      if (!$('paletteOverlay').hidden) return;
      $('paletteOverlay').hidden = false;
      // 命令面板与抽屉同样禁用折叠按钮（同一个右上角误触坑）
      syncCollapseGuard();
      // 命令面板是居中浮层，与工作台区域重叠；原生工作台 webview 盖在所有 HTML
      // 之上（macOS 独立 NSWindow），必须像抽屉一样显式隐藏，否则面板被盖住
      // 看不见（用户反馈「点管理没出现操作页面」的唯一原因）。
      invoke('hide_workbench_cmd').catch(() => {});
      $('paletteInput').value = '';
      paletteSel = 0;
      renderPalette('');
      $('paletteInput').focus({ preventScroll: true });
    });
  }
  function closePalette(restoreWorkbench = true) {
    clearToasts();
    overlayGen++; // 作废在途的显示回调
    paletteOpen = false;
    $('paletteOverlay').hidden = true;
    if ($('paletteOverlay').contains(document.activeElement)) document.activeElement.blur();
    syncCollapseGuard();
    // 恢复工作台（受守卫：抽屉/面板仍占用时不恢复；runPaletteItem → goRegion
    // 还会再开抽屉，两次 invoke 按发出顺序执行，最终态正确）。
    if (restoreWorkbench) maybeShowWorkbench();
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
  // App 级组合键统一入口（MOD+K / MOD+1–4）。两个来源共用：
  // 1) 壳页自身的 window keydown（焦点在壳页时）；
  // 2) 工作台 webview 转发来的 shell:shortcut 事件（焦点在工作台时——它是独立
  //    原生 webview，按键不会冒泡到壳页，由注入脚本 + capability 转发）。
  function handleAppCombo(key) {
    // 首次安装引导期间不响应 App 级组合键/菜单「管理面板」：引导层 z-index 最高，
    // 此时打开命令面板会被它盖住（用户看不见，但它确实打开了，还会 hide_workbench），
    // 等安装完成、引导层撤掉就"凭空"露出一个面板 + 工作台被压住（用户实测）。
    if (setupActive) return;
    if (key === 'k') {
      if (paletteOpen) cycleRegionSelection();
      else openPalette();
      return;
    }
    if (['1', '2', '3', '4'].includes(key)) {
      // 面板开着时先收起，否则面板与抽屉两个浮层叠加、且工作台显隐会打架
      if (paletteOpen) closePalette();
      goRegion(REGIONS[Number(key) - 1].id);
    }
  }
  // 工作台转发通道（workbench.rs SHORTCUT_FORWARD_JS → shell:shortcut）
  T.event.listen('shell:shortcut', (e) => {
    const k = String(e.payload || '').toLowerCase();
    if (k === 'k' || ['1', '2', '3', '4'].includes(k)) handleAppCombo(k);
  }).catch(() => {});
  $('paletteInput').addEventListener('input', function () { paletteSel = 0; renderPalette(this.value); });
  $('paletteInput').addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (!paletteItems.length) return; // 空结果时取模会得到 NaN
      paletteSel = (paletteSel + 1) % paletteItems.length;
      renderPalette($('paletteInput').value);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (!paletteItems.length) return;
      paletteSel = (paletteSel - 1 + paletteItems.length) % paletteItems.length;
      renderPalette($('paletteInput').value);
    }
    else if (e.key === 'Enter') { e.preventDefault(); runPaletteItem(paletteSel); }
    else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closePalette(); }
  });
  $('paletteOverlay').addEventListener('click', (e) => { if (e.target === $('paletteOverlay')) closePalette(); });
  $('btnManage').addEventListener('click', openPalette);
  // 系统菜单 ⌘K（Rust 侧「视图 → 管理面板」菜单项）的入口：焦点在工作台 webview
  // 时壳页收不到 keydown，由菜单快捷键转发到这里。走 handleAppCombo 以共用
  // 「引导期间不响应」等守卫。
  window.__openManagePalette = () => handleAppCombo('k');

  // ---------------- 确认弹窗（modal，供插件卸载/版本操作/更新确认复用） ----------------
  const modalEl = $('modal');
  let modalAccept = null;
  function openModal(opts) {
    // 确认弹窗是壳页内的整幅浮层（#modal，fixed inset:0），而原生工作台永远盖在 HTML 之上：
    // 从命令面板触发时，runPaletteItem 会先 closePalette() → maybeShowWorkbench() 把工作台
    // 移回窗口并把壳页裁到 36pt（顶栏），弹窗就只剩那一条、看不见也点不到（实测定位）。
    // 因此弹窗和抽屉/命令面板一样是「占用工作区」的浮层：打开时让位，关闭时再归位。
    invoke('hide_workbench_cmd').catch(() => {});
    $('modalTitle').textContent = opts.title || '确认操作';
    $('modalMsg').textContent = opts.message || '';
    $('modalIcon').className = 'modal-icon' + (opts.danger ? ' danger' : '');
    $('modalIcon').innerHTML = opts.danger ? ICON.trash : ICON.alert;
    $('modalOk').textContent = opts.okLabel || '确定';
    $('modalOk').className = 'btn' + (opts.danger ? ' danger' : '');
    $('modalNo').textContent = opts.noLabel || '取消';
    modalAccept = opts.onAccept || null;
    modalEl.hidden = false;
    $('modalOk').focus({ preventScroll: true });
  }
  function closeModal(restoreWorkbench = true) {
    modalEl.hidden = true;
    modalAccept = null;
    // 与抽屉/命令面板同规：浮层收起后恢复工作台（受守卫——抽屉/面板仍占用时不动作，
    // 所以「从抽屉里弹确认」这条路径依然安静）。
    if (restoreWorkbench) maybeShowWorkbench();
  }
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
    // ⌘K / ⌘1–4（Windows 为 Ctrl+K / Ctrl+1–4）
    if ((e.metaKey || e.ctrlKey) && !e.altKey) {
      const k = e.key.toLowerCase();
      if (k === 'k' || ['1', '2', '3', '4'].includes(k)) {
        e.preventDefault();
        handleAppCombo(k);
        return;
      }
    }
    // Esc：面板 → 抽屉 → 确认弹窗 逐级关闭
    if (e.key === 'Escape') {
      // 最上层优先：确认弹窗 / Registry 弹窗（z-index 最高）→ 命令面板 → 抽屉。
      // 旧顺序先关抽屉，导致「弹窗开着按 Esc 关掉了背后的抽屉、弹窗不动」。
      if (!modalEl.hidden) { closeModal(); return; }
      if (!$('registryModal').hidden) { closeRegistryModal(); return; }
      if (paletteOpen) { closePalette(); return; }
      if ($('drawer').classList.contains('open')) { closeDrawer(); return; }
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
  let setupListVer = ''; // 下拉里选中的版本；手输清空后回落到它（否则"框是空的、实际还装旧值"）
  let setupCancelled = false;
  let setupProgressTimer = null; // 安装进度轮询（setup_state_cmd 兜底）
  let waitBusyReset = false; // 撞 BUSY 分支：等后端收尾结束后回初始页
  let setupFetchCount = 0;
  let setupLastProgress = null; // 模块级去重：事件与轮询双通道共用，防同一行双计
  let setupRunId = 0; // 安装运行令牌：取消/失败的旧 run 不得覆盖新 run 的 UI
  let setupStarting = false; // 预检/启动期防重入（连点会并发两个 runSetup → 轮询 timer 泄漏）

  function showSetupView() {
    setupActive = true;
    // 引导层是阻塞式最高层：进入时收起可能开着的浮层，避免"装完才冒出来"
    closeOverlays();
    hidePlaceholder();
    setupOverlay.hidden = false;
    loadSetupVersions(); // 预填版本下拉（失败仅提示，不阻塞安装主流程）
  }
  function hideSetupView() {
    setupActive = false;
    setupOverlay.hidden = true;
    clearInterval(setupProgressTimer); // 进度轮询停止
    if (!workbenchReady) showPlaceholder(); // 引导收起但工作台未就绪：恢复占位 spinner
    // 引导结束即让工作台归位（受守卫：浮层占用时不动作）——否则装完仍是壳页背景
    maybeShowWorkbench();
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
      // 只列最近 5 个可安装版本（用户要求；更旧的可在管理抽屉输入版本号安装）
      list.slice(0, 5).forEach((v) => {
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
    setupListVer = v;
    setupVerValue = v;
    setupVerLabel.textContent = 'v' + v;
    syncSetupVerDisplay(v);
    // 用户从列表里选（silent=false）才清空手输框：runSetup 里手输值优先于列表值，
    // 留着旧手输值会让人以为装的是刚选的那个版本（用户反馈）。静默刷新（silent=true，
    // 如「刷新版本」重新拉取列表 / 列表预填）**不能**动用户手输值——那会把用户刚敲的
    // 版本号吃掉、装成列表首项。
    if (!silent) {
      verManual.value = '';
      closeSetupVerMenu();
    }
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
  // 手输版本：优先展示（联动主区），不清空下拉 label（下拉仅记录选中态）。
  // 清空输入框 → 回落到下拉里选中的版本（或列表默认的最新版）：此前清空后
  // setupVerValue 仍是旧的手输值，界面显示"将由列表决定"、实际却装的是那个旧值。
  verManual.addEventListener('input', function () {
    const v = this.value.trim();
    if (v) {
      setupVerValue = v;
      syncSetupVerDisplay(v.startsWith('v') ? v.slice(1) : v);
      return;
    }
    if (setupListVer) {
      setupVerValue = setupListVer;
      syncSetupVerDisplay(setupListVer);
    } else {
      setupVerValue = '';
      setupVerShow.textContent = '—'; // 连列表都没获取到：显示"未定"
    }
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
        // 必须把引导浮层重新显示出来：showSetupError 只切子视图、不负责容器显隐。
        // 否则错误文案与「重试」按钮都藏在 hidden 的浮层里，用户只看到占位 spinner
        // 一直转（死路，只能强杀应用）。
        setupActive = true;
        setupOverlay.hidden = false;
        hidePlaceholder();
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
  let dshLatestVer = null; // 后端查询到的 latest（npm dist-tag；可能落后于已发布版本）
  // 「最新」以 **registry 版本列表里的 semver 最大值** 为准，而不是 dist-tag：实测上游
  // dist-tags.latest 落后于最新已发布版本（如 latest=0.1.5-rc.1 而列表已有 0.1.6-alpha.1）
  // —— 用 dist-tag 会导致
  // 列表首行没有「最新」徽标、甚至提示「发现新版本」把用户往旧版本上带（降级）。
  let dshNewestVer = null;
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
      // 版本列表里的最大版本（不假定后端顺序，自己按 cmpVer 取最大；列表为空时回退 dist-tag）
      const versions = st.versions || [];
      // registry 源的版本键未经过后端校验（自定义源可能含 "0.1" 这类非法键），
      // 取最大前先按宽松 semver 过滤——否则可能选出后端会拒绝安装的版本。
      const SEMVERISH = /^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$/;
      const newest = versions
        .filter((v) => SEMVERISH.test(String(v)))
        .reduce((m, v) => (!m || cmpVer(v, m) > 0 ? v : m), null);
      dshNewestVer = newest || dshLatestVer;
      dshCurrentEl.textContent = current;
      // 未安装时不要显示「当前」徽标（否则出现「未安装 + 当前」自相矛盾）
      const curBadge = $('dshCurrentBadge');
      if (curBadge) curBadge.hidden = !st.current || st.current === '未安装';

      // 用 semver 比较而不是「字符串不等」：dist-tag 落后时不能把旧版本当成「新版本」
      const hasUpdate = !!dshNewestVer && current !== '未安装' && cmpVer(dshNewestVer, current) > 0;
      dshUpdateBanner.hidden = !hasUpdate;
      if (hasUpdate) dshLatestEl.textContent = 'v' + dshNewestVer;
      // 「安装中」或「App 更新中」都要禁用——refreshDsh 会在切分段时重渲染，
      // 若只按 st.installing 判断，更新期间切到 dsh 分段会把刚锁上的按钮放出来。
      btnUpdateDshEl.disabled = !!st.installing || appUpdating;
      btnCheckDshEl.disabled = !!st.installing || appUpdating;
      btnCheckDshEl.textContent = st.installing ? '安装中…' : '检查更新';

      // 第 3 个参数是「哪一行打『最新』徽标」——传 semver 最大值（不是 dist-tag）
      renderVersions(versions, current, dshNewestVer, !!st.installing || appUpdating, st.installed || []);

      if (st.installing) {
        setDshStatus('正在安装新版本…安装完成后工作台自动重启', 'run');
      } else if (hasUpdate) {
        setDshStatus('发现新版本 v' + dshNewestVer + '，可更新', 'acc');
      } else if (current === '未安装') {
        setDshStatus('尚未安装 dsh，可安装 v' + dshNewestVer + '（或指定版本）', 'acc');
      } else if (!dshNewestVer) {
        // 版本列表为空（离线或检查失败）时无法判断，如实提示
        setDshStatus('暂无法确认最新版本（离线或启动时检查失败）', 'warn');
      } else {
        setDshStatus('已是最新版本', 'ok');
      }
    } catch (e) {
      setDshStatus('读取 dsh 状态失败：' + (e.message || e), 'err');
    }
  }

  // 语义化版本比较：主版本三段 + 预发布段。旧实现用 Number() 解析「0.1.1-rc.2」
  // 会得到 NaN（按 0 处理），使「回滚目标」在 rc 版本之间可能选错。
  function cmpVer(a, b) {
    const [am, ap] = String(a).split('-');
    const [bm, bp] = String(b).split('-');
    const A = String(am).split('.').map(Number), B = String(bm).split('.').map(Number);
    for (let i = 0; i < 3; i++) {
      const x = A[i] || 0, y = B[i] || 0;
      if (x !== y) return x - y;
    }
    // 正式版 > 预发布版；两个预发布版按标识符逐段比较（纯数字按数值，否则字典序，
    // 数字段优先级低于字母段——与 semver 一致）
    if (!ap && !bp) return 0;
    if (!ap) return 1;
    if (!bp) return -1;
    const as = String(ap).split('.'), bs = String(bp).split('.');
    for (let i = 0; i < Math.max(as.length, bs.length); i++) {
      const x = as[i], y = bs[i];
      if (x === undefined) return -1;
      if (y === undefined) return 1;
      const nx = /^\d+$/.test(x), ny = /^\d+$/.test(y);
      if (nx && ny) { const d = Number(x) - Number(y); if (d) return d; }
      else if (nx !== ny) return nx ? -1 : 1;
      else if (x !== y) return x < y ? -1 : 1;
    }
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
    // 只显示最近 5 个版本（用户要求；更旧的可用下方输入框指定版本号安装）
    const slice = versions.slice(0, 5);
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
    // App 更新进行中：dsh 的装/切/回滚会与更新后的重启抢状态，直接拒绝
    if (appUpdating) { toast('应用正在更新，请稍候再操作', 'err'); return; }
    const opCn = op === 'rollback' ? '回滚' : op === 'switch' ? '切换' : op === 'update' ? '更新' : '安装';
    const doing = op === 'rollback' ? '回滚中…' : op === 'switch' ? '切换中…' : op === 'update' ? '更新中…' : '安装中…';
    // 记下原按钮文案：结束后回写它而不是 opCn —— 抽屉「更新到最新」按钮的文案是
    // 「更新到最新」，用 opCn（"更新"）回写会让它在任何一次更新结束后静默变短。
    const btnLabel = btn ? btn.textContent : null;
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
    if (btn) { btn.disabled = false; btn.textContent = btnLabel || opCn; }
  }

  function checkDsh() { refreshDsh(); }
  btnCheckDshEl.addEventListener('click', checkDsh);
  btnUpdateDshEl.addEventListener('click', () => {
    if (!dshNewestVer) return;
    const v = dshNewestVer; // 装「版本列表里的最大值」，别装可能落后的 dist-tag
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
    input.focus({ preventScroll: true });
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
    // 单行也封顶：有些包（postinstall / 打包日志）一次就打出一行几十 KB——行数虽有上限，
    // 超长单行仍会让节点文本与重排代价无界增长。
    const MAX_LINE = 2000;
    if (text.length > MAX_LINE) text = text.slice(0, MAX_LINE) + ` …（本行过长已截断，原长 ${text.length}）`;
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
    // 输出无上限：pnpm 长输出可达上万行，节点与内存会无界增长（且每行都写一次
    // scrollTop 触发同步重排）。只保留最近 500 行。
    while (pluginLogEl.childElementCount > 500) pluginLogEl.removeChild(pluginLogEl.firstChild);
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
        title.title = p.name; // 名称过长时行内省略号截断，hover 看全
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
        // 来源 + 名称：npm 显示版本 spec、Git/URL 显示来源类型、本地插件显示清理后的
        // 绝对路径（后端 list_installed_plugins 分类）。可能很长 → 行内省略号截断，
        // 完整值挂 title 供 hover 查看。
        meta.textContent = p.source_label || (p.version ? 'v' + p.version : '');
        meta.title = meta.textContent;
        row.appendChild(meta);
        if (p.installed) {
          const btn = document.createElement('button');
          btn.className = 'btn danger-ghost sm';
          btn.textContent = '卸载';
          btn.disabled = appUpdating; // 重渲染出来的按钮也要继承"更新中"的锁定
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
    if (appUpdating) { toast('应用正在更新，请稍候再操作', 'err'); return; }
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
    if (appUpdating) { toast('应用正在更新，请稍候再操作', 'err'); return; }
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

  // ---------------- App 更新进行中的 UI 状态锁 ----------------
  // 规则：App 更新一旦开始，**一切会改动 App 或 dsh 状态的按钮**都禁用（检查更新 / 下载 /
  // 卸载两档 / dsh 分段的安装·切换·回滚·更新）；只读与逃生动作保留（在浏览器打开下载页、
  // 仓库链接、切分段、关抽屉）——更新结束时本 App 会退出并由新版接管，期间任何"写状态"的
  // 操作都可能被拦腰打断（dsh 装到一半留 tmp、卸载与更新互相拆台）。正确性另有后端并发门兜底。
  let appUpdating = false;
  let appUpdateLockedEls = [];
  // 「有新版」的两级提示：顶栏「管理」上的呼吸灯（唯一始终可见的区域——工作台盖住壳页时
  // 壳页只剩 36pt 顶栏）+ 关于面板顶部横幅 + 关于 tab 上的小点。三者由这一个函数统一决定，
  // 避免状态散落后出现「红点还在但已经在装」这类矛盾；更新进行中一律隐藏（进度条在关于面板）。
  function renderAppUpdateNotice() {
    const show = !!appLatest && !appUpdating;
    const dot = $('manageDot'), tabDot = $('aboutTabDot'), banner = $('appUpdateBanner');
    if (dot) dot.hidden = !show;
    if (tabDot) tabDot.hidden = !show;
    if (banner) banner.hidden = !show;
    const latestEl = $('appUpdateLatest');
    if (latestEl && show) latestEl.textContent = 'v' + appLatest;
  }
  function appUpdateLockTargets() {
    return [
      btnCheckAppEl,
      btnDownloadAppEl,
      $('btnUninstallKeep'),
      $('btnUninstallWipe'),
      ...document.querySelectorAll('[data-section="dsh"] button'),
      ...document.querySelectorAll('[data-section="plugins"] button'),
    ].filter(Boolean);
  }
  function setAppUpdating(v) {
    appUpdating = !!v;
    const prog = $('appProgress'), txt = $('appProgressText'), fill = $('appProgressFill');
    if (appUpdating) {
      appUpdateLockedEls = appUpdateLockTargets().map((el) => ({ el, was: el.disabled }));
      appUpdateLockedEls.forEach(({ el }) => { el.disabled = true; });
      if (prog) { prog.hidden = false; }
      if (fill) { fill.style.width = '0%'; }
      if (txt) { txt.hidden = false; txt.textContent = '准备下载…'; }
      renderAppUpdateNotice(); // 更新中：隐藏红点与横幅（进度条已在关于面板显示）
    } else {
      // 逐个恢复"锁之前的禁用状态"（否则会把本该禁用的按钮放出来，例如已是最新时的下载按钮）
      appUpdateLockedEls.forEach(({ el, was }) => { el.disabled = was; });
      appUpdateLockedEls = [];
      if (prog) prog.hidden = true;
      if (txt) { txt.hidden = true; txt.textContent = ''; }
      // 锁定期间重渲染出来的版本行按钮是按"更新中"渲染成禁用的，快照恢复管不到它们
      // （元素是新建的）——解锁后重渲染一次，让它们回到按真实 registry 状态渲染。
      refreshDsh();
      renderAppUpdateNotice(); // 更新结束后按当前 appLatest 重新显示（失败时横幅要回来）
    }
  }
  // 下载/校验进度（Rust 只在整数百分比变化时发一次）
  T.event.listen('app:update-progress', (e) => {
    // 解锁后迟到的残留事件不得把状态行写回"更新中"（事件与命令返回的到达顺序无保证）
    if (!appUpdating) return;
    const p = (e && e.payload) || {};
    const prog = $('appProgress'), fill = $('appProgressFill'), txt = $('appProgressText');
    if (!txt) return;
    const mb = (b) => (Number(b) / 1048576).toFixed(1) + 'MB';
    if (p.phase === 'install') {
      if (prog) prog.hidden = true;
      // Windows：应用会**自己先退出**、由更新助手在退出后安装（退出是预期行为，不是崩溃）；
      // macOS：装完延迟重启。文案要如实说明，避免用户以为要守着窗口。
      txt.textContent = '正在安装…应用即将自动退出，安装完成后会自动重启';
      setAppStatus('正在安装…', 'run');
      return;
    }
    const total = typeof p.total === 'number' && p.total > 0 ? p.total : null;
    if (total) {
      const pct = Math.min(100, Math.round((Number(p.downloaded) / total) * 100));
      if (prog) { prog.hidden = false; prog.setAttribute('aria-valuenow', String(pct)); }
      if (fill) fill.style.width = pct + '%';
      txt.textContent = '正在下载更新… ' + pct + '%（' + mb(p.downloaded) + ' / ' + mb(total) + '）';
      setAppStatus('正在下载更新… ' + pct + '%', 'run');
    } else {
      if (prog) prog.hidden = true;
      txt.textContent = '正在下载更新… 已下载 ' + mb(p.downloaded);
      setAppStatus('正在下载更新…', 'run');
    }
  }).catch(() => {});

  async function checkApp() {
    if (appUpdating) return; // 更新进行中：按钮已禁用，这里再兜一层
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
      renderAppUpdateNotice();
      } else {
        appLatest = null;
        btnDownloadAppEl.disabled = true;
        btnDownloadAppEl.textContent = '下载并安装更新';
        setAppStatus('已是最新版本', 'ok');
      renderAppUpdateNotice();
      }
    } catch (e) {
      setAppStatus('检查更新失败：' + (e.message || e), 'err');
    } finally {
      btn.disabled = false;
      btn.textContent = '检查更新';
    }
  }
  btnCheckAppEl.addEventListener('click', checkApp);
  // 启动探测：壳页**加载与重载**都会调（后端保证每次启动至多查一次；结果缓存在后端，
  // 所以 reload 后红点/横幅不会丢）。有新版 → 亮红点 + 关于面板横幅。
  invoke('app_update_probe_cmd')
    .then((v) => { if (v) { appLatest = v; renderAppUpdateNotice(); } })
    .catch(() => {});
  // 后台检查完成后由后端广播（含"无新版"的 null：据此熄灭红点与横幅）
  T.event.listen('app:update-available', (e) => {
    appLatest = (e && e.payload) || null;
    renderAppUpdateNotice();
  }).catch(() => {});
  // 「下载并安装」的唯一入口：关于面板主按钮与顶部横幅按钮共用（两份流程必然走岔）
  async function startAppInstall() {
    if (appUpdating) return; // 双保险：按钮此时已禁用
    setAppUpdating(true);
    setAppStatus('正在下载并安装更新…安装完成后应用将自动重启');
    const tid = 'app-update';
    taskStart(tid, '下载应用更新');
    taskUpdate(tid, '正在下载并安装…');
    try {
      await invoke('app_update_cmd');
      // 成功即退出当前实例（安装器/新版负责启动）；任务态到此为止
      taskFinish(tid, 'ok', '更新包已就绪，应用即将退出并安装');
    } catch (e) {
      setAppUpdating(false);
      taskFinish(tid, 'err', (e && e.message) || String(e));
      setAppStatus('更新失败：' + ((e && e.message) || e), 'err');
      btnDownloadAppEl.disabled = !appLatest;
    }
  }
  btnDownloadAppEl.addEventListener('click', startAppInstall);
  $('btnAppUpdateNow').addEventListener('click', startAppInstall);
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
    if (appUpdating) { toast('应用正在更新，请稍候再操作', 'err'); return; }
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