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

  // ---------------- 导航栏折叠（收起 Tab 栏扩大工作区，状态记忆） ----------------
  const titlebar = $('titlebar');
  const btnTabsToggle = $('btnTabsToggle');
  function applyTabsCollapsed(c) {
    titlebar.classList.toggle('collapsed', c);
    // 顶栏高度联动 workspace 与把手（.workarea top / .tb-toggle top 均用
    // var(--dsh-topbar-h)，transition 同步产生伸缩动画）
    document.documentElement.style.setProperty('--dsh-topbar-h', c ? '28px' : '46px');
    btnTabsToggle.classList.toggle('collapsed', c);  // 控制 CSS 三角朝向
    btnTabsToggle.title = c ? '展开导航栏' : '收起导航栏';
    btnTabsToggle.setAttribute('aria-expanded', String(!c));
  }
  btnTabsToggle.addEventListener('click', () => {
    const next = !titlebar.classList.contains('collapsed');
    applyTabsCollapsed(next);
    try { localStorage.setItem('tabsCollapsed', next ? '1' : '0'); } catch (e) { /* 忽略 */ }
  });
  try {
    applyTabsCollapsed(localStorage.getItem('tabsCollapsed') === '1');
  } catch (e) { applyTabsCollapsed(false); }

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
    // 重复点击已选中的 tab：无操作（否则 dsh 等会重复 refreshDsh 重渲染，
    // 用户看到「页面一直刷新」）
    const cur = [...tabs].find((t) => t.classList.contains('active'));
    if (cur && cur.dataset.tab === name) return;
    tabs.forEach((t) => t.classList.toggle('active', t.dataset.tab === name));
    tabOf(name);
  }
  tabs.forEach((t) => t.addEventListener('click', () => selectTab(t.dataset.tab)));

  // V7：双击「工作台」tab → 系统浏览器打开当前工作台（URL 未就绪时后端静默忽略）
  const workbenchTab = [...tabs].find((t) => t.dataset.tab === 'workbench');
  if (workbenchTab) {
    workbenchTab.addEventListener('dblclick', () => {
      invoke('open_workbench_url_cmd').catch(() => {});
    });
  }

  // ⌘K / Ctrl+K：循环切换；Esc：管理页返回工作台
  window.addEventListener('keydown', (e) => {
    // V4：Cmd/Ctrl+C 复制选中文字（仅壳页：输入框内走浏览器默认；
    // iframe 内焦点时 keydown 不会冒泡到父文档，天然不拦截 iframe 内复制）
    if ((e.metaKey || e.ctrlKey) && (e.key === 'c' || e.key === 'C')) {
      const t = e.target;
      if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable)) return;
      const sel = document.getSelection();
      const text = sel ? sel.toString() : '';
      if (!text) return;
      e.preventDefault();
      navigator.clipboard.writeText(text).catch(() => {});
      return;
    }
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

  function loadWorkbench(url, force) {
    // force：安装成功后主动恢复（等价「点重试」）——即使 URL 与 lastUrl 相同
    // 也要重载 iframe（更新/重装场景 lastUrl 可能已等于新 URL，防重入会拦住）。
    if (!url || (!force && url === lastUrl)) return;
    lastUrl = url;
    // dsh:url 事件 = 工作台就绪（引导安装成功后 boot 会发）→ 收起引导视图
    if (setupActive) hideSetupView();
    // 自动切回工作台 tab：**仅安装/更新主动恢复（force）时**——用户在 dsh tab
    // 点安装,装完要看到工作台加载;纯换端口/后台重启(dsh:url 事件)不带 force,
    // 不打扰用户当前所在的 tab（避免把正在 dsh/plugins 操作的用户强拉回工作台）。
    if (force) selectTab('workbench');
    showPlaceholder(); // 换端口/重载路径：先盖住，onload 后撤
    wb.src = url;
    wb.focus();
  }

  // V6：点击品牌（图标+App名）刷新工作台（等同右键 reload）；未就绪时无操作
  const brandEl = $('brand');
  brandEl.addEventListener('click', () => { if (lastUrl) loadWorkbench(lastUrl, true); });
  brandEl.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); if (lastUrl) loadWorkbench(lastUrl, true); } });

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
    //    监听器之后常驻,用于捕获工作台换端口/重启后的 dsh:url(有意保留,非泄漏)。
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
  const setupVerInput = $('setupVerInput');
  const setupAdv = $('setupAdv');
  let setupCancelled = false;
  // 装完等待工作台就绪态：此时「取消」按钮语义为「放弃等待」（回初始页、不调
  // setup_cancel_cmd——安装已完成无进程可取消）；区别于安装中的「取消安装」。
  let setupInstalled = false;
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

  let setupRunId = 0; // 安装运行令牌：取消/失败的旧 run 不得覆盖新 run 的 UI
  let setupStarting = false; // 预检/启动期防重入（连点会并发两个 runSetup → 轮询 timer 泄漏）
  async function runSetup() {
    if (setupStarting) return;
    setupStarting = true;
    const myRun = ++setupRunId;
    const inputVer = setupVerInput.value.trim();
    const ver = inputVer || setupVer.value;
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
      // 成功：后端 boot 已在后台拉起 dsh（冷启动约 5-30s）。**不依赖 dsh:url
      // 事件**（本环境实测会丢失）——装完后让 startSetupProgressPolling 继续跑，
      // 它轮询 dsh_url 就绪 → loadWorkbench（现在会自动切回工作台 tab）。
      // 不设 30s 定时报错兜底：dsh 刚装完正在冷启动,主动 showSetupError 会打断
      // 一个本可能成功的启动（用户判定该兜底不合理）——只需耐心等到 dsh_url 就绪。
      if (!setupCancelled) {
        markSetupInstalled(); // 工作台正在启动 + 取消按钮「放弃等待」（闭环出口）
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
  let waitBusyReset = false; // 撞 BUSY 分支：等后端收尾结束后回初始页
  function startSetupProgressPolling() {
    clearInterval(setupProgressTimer); // 幂等：先清旧 timer，防旧 run 残留泄漏/误杀新 timer
    setupProgressTimer = setInterval(async () => {
      if (!setupActive || setupCancelled) { clearInterval(setupProgressTimer); return; }
      try {
        const st = await invoke('setup_state_cmd');
        if (st.dsh_url) {
          waitBusyReset = false;
          clearInterval(setupProgressTimer);
          // force=true：绕过 same-URL 守卫（重装/更新可能复用上次端口,URL 与
          // lastUrl 相同）,并自动切回工作台 tab（装完可来自 dsh tab 触发）。
          loadWorkbench(st.dsh_url, true);
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
    setupInstalled = false; // 退出"装完等待"态（装完后点"放弃等待"也走这里回初始）
    waitBusyReset = false;
    clearInterval(setupProgressTimer); // 幂等：所有回初始的路径都停轮询，防空转泄漏
    setupProgress.hidden = true;
    setSetupPhase('stage');            // 隐藏错误视图，显示 stage 区
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

  // 装完成后进入"等待工作台就绪"态：引导页显示"工作台正在启动"，取消按钮
  // 变为「放弃等待」（点它回初始页重装/重试，不调 setup_cancel_cmd——无进程可取消）。
  // runSetup（引导页安装）与 updateDsh（dsh tab 安装,首次同步引导）装完共用,
  // 避免两处重复；就绪后 startSetupProgressPolling 检测 dsh_url → loadWorkbench。
  function markSetupInstalled() {
    setupInstalled = true;
    setupStage.textContent = '工作台正在启动，请稍候…';
    // 明确出口预期：不自动报错打断（避免冷启动中误报），但提示长时间无响应可放弃。
    setupMeta.textContent = '冷启动约需 5-30 秒；若长时间未进入工作台，可点「放弃等待」返回重试';
    btnSetupCancel.disabled = false;
    btnSetupCancel.textContent = '放弃等待';
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
        if (url) { loadWorkbench(url, true); return; } // force:重试直接拉起并切回工作台
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
    // 装完等待工作台就绪态：取消=「放弃等待」，回初始页（安装已完成,无进程
    // 可取消,不调 setup_cancel_cmd、不走"正在取消"流程）。
    if (setupInstalled) { resetSetupInitial(); return; }
    setupCancelled = true;
    btnSetupCancel.disabled = true;
    btnSetupCancel.textContent = '正在取消…';
    setupStage.textContent = '正在取消安装…';
    invoke('setup_cancel_cmd').catch(() => {});
  });

  // 进度：阶段文本（setupStage）+ npm fetch 行计数（setupMeta 显示"已下载 N 个
  // 依赖包"，不刷屏）；进度条为不确定态流动动画。
  let setupFetchCount = 0;
  let setupLastProgress = null; // 模块级去重：事件与轮询双通道共用，防同一行双计
  function applySetupProgress(text) {
    if (!text) return;
    if (text === setupLastProgress) return; // 双通道同一行只处理一次
    setupLastProgress = text;
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

      renderVersions(st.versions || [], current, dshLatest, !!st.installing, st.installed || []);

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

  // 版本列表：后端已收敛为最近 10 个（get_dsh_state.versions），前端不再本地过滤/缓存。
  function renderVersions(versions, current, latest, installing, installed = []) {
    dshVersionsEl.innerHTML = '';
    if (!versions.length) {
      const empty = document.createElement('div');
      empty.className = 'empty';
      empty.textContent = '未获取到可用版本（检查网络或 Registry 源）';
      dshVersionsEl.appendChild(empty);
      return;
    }
    // 兜底截取最近 10 个（后端已收敛，这里防御性再截一次）。
    const slice = versions.slice(0, 10);
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
        // 已安装（本地目录存在,非当前）→「切换」（走 install_version 复用,秒切）；
        // 未安装 →「安装」。点按都调 updateDsh(v)（后端会自动复用已存在目录）。
        btn.textContent = installed.includes(v) ? '切换' : '安装';
        btn.addEventListener('click', () => { btn.disabled = true; updateDsh(v); });
        btn.disabled = installing;
      }
      row.appendChild(btn);
      return row;
    };
    slice.forEach((v) => dshVersionsEl.appendChild(mkRow(v)));
    // 默认仅展示最近 10 个的提示
    const tip = document.createElement('div');
    tip.className = 'empty';
    if (versions.length > slice.length) {
      tip.textContent = '共 ' + versions.length + ' 个版本，默认展示最近 ' + slice.length + ' 个；可输入版本号安装任意已发布版本';
    }
    if (tip.textContent) dshVersionsEl.appendChild(tip);
  }

  async function updateDsh(ver) {
    // 点击即锁定全部安装入口（防连点/其他版本并发）：版本行按钮、更新到最新、
    // 检查更新（hero 右侧不空）。finally 里 refreshDsh 按后端 installing 恢复。
    setDshStatus('正在安装 dsh v' + ver + '…', 'run');
    // 首次安装（未装闭包、workbench 引导页在显示）时,同步 workbench 引导页
    // 进入"安装中"进度态——否则用户在 dsh tab 点安装,切回工作台仍是初始
    // "准备 dsh 运行时"界面（应用状态不同步 bug）。安装完成由引导进度轮询
    // 检测 dsh_url 就绪 → loadWorkbench → hideSetupView 自动收起引导。
    // syncSetup 记录是否同步过引导,供失败/异常路径正确清理（防轮询泄漏/卡死）。
    const syncSetup = (!lastUrl && setupView.hidden === false);
    if (syncSetup) {
      setupActive = true;
      setSetupPhase('stage');
      setupStage.textContent = '正在安装 dsh v' + ver + '…';
      setupMeta.textContent = '首次安装需下载约 300MB 依赖包，通常需要几分钟，请耐心等待（可随时取消）';
      setupProgress.hidden = false;
      btnSetupInstall.disabled = true; // 锁定引导页安装按钮,防与 dsh tab 安装并发(runSetup 同款保护)
      btnSetupCancel.hidden = false;
      btnSetupCancel.disabled = false;
      btnSetupCancel.textContent = '取消安装';
      setupCancelled = false;
      setupFetchCount = 0;
      setupLastProgress = null;
      startSetupProgressPolling();
    }
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
      // 后端安装成功 → 自动重启工作台（dsh:url 事件驱动 iframe 重载）。
      // 首次安装同步过引导时,由 syncSetup 已启动的 startSetupProgressPolling
      // 检测 dsh_url 就绪 → loadWorkbench（自动切回工作台）；不设 30s 定时报错
      // 兜底（同 runSetup,不打断正在冷启动的工作台）。
      setDshStatus('工作台正在启动，请稍候…', 'ok');
      // 首次安装同步过引导:若用户已切回 workbench 点了「取消安装」(setupCancelled),
      // 轮询会因 setupCancelled 自停——回初始页而非停在"工作台正在启动"无检测通道；
      // 否则 markSetupInstalled 进入等待态(轮询继续等 dsh_url → loadWorkbench 切回)。
      if (syncSetup) {
        if (setupCancelled) { resetSetupInitial(); }
        else { markSetupInstalled(); }
      } else {
        // 非 syncSetup(工作台已在使用):装完不依赖会丢失的 dsh:url 事件——主动轮询
        // get_dsh_url(restart_dsh 已清 DSH_URL,新 dsh 写入后命中)再 loadWorkbench(force)
        // 重载 iframe,否则事件丢失时工作台会停在旧版本。
        // 窗口会被 restart_dsh 隐藏但不销毁,此轮询在隐藏窗口内继续运行。
        (async () => {
          for (let i = 0; i < 38; i++) {
            try {
              const url = await invoke('get_dsh_url');
              if (url) { loadWorkbench(url, true); return; }
            } catch (e) { /* 单次失败忽略 */ }
            await new Promise((r) => setTimeout(r, 800));
          }
          // 轮询耗尽(≈30s)仍无 url:提示可重试,不无限等(窗口隐藏,回到 dsh tab 可见)
          setDshStatus('工作台启动超时，可点击「更新到最新」重试', 'err');
        })();
      }
    } catch (e) {
      clearInterval(progressTimer);
      // 用户主动取消（syncSetup 时在 workbench 点了「取消安装」）：与 runSetup 的 catch
      // 一致——回初始页、不显示「安装失败」（取消不是失败）。否则停引导轮询并恢复引导页。
      if (syncSetup && setupCancelled) { clearInterval(setupProgressTimer); resetSetupInitial(); setDshStatus('已取消安装', ''); return; }
      // 首次安装同步过引导时,失败要停引导进度轮询并恢复引导页,否则 setupProgressTimer
      // 永久空转、setupView 卡"安装中"（runSetup 有此兜底,updateDsh 之前缺失）。
      if (syncSetup) { clearInterval(setupProgressTimer); resetSetupInitial(); }
      setDshStatus('安装失败：' + (e.message || e), 'err');
    } finally {
      refreshDsh(); // 复位 installing 状态并重渲染列表
    }
  }

  // 输入版本号安装：先校验版本存在（避免不存在的版本白白触发大下载），再走 updateDsh
  const dshVersionInput = $('dshVersionInput');
  const btnInstallVersion = $('btnInstallVersion');
  btnInstallVersion.addEventListener('click', async () => {
    const ver = dshVersionInput.value.trim();
    if (!ver) { setDshStatus('请输入版本号', 'err'); return; }
    btnInstallVersion.disabled = true;
    try {
      const exists = await invoke('version_exists_cmd', { ver });
      if (!exists) { setDshStatus('版本 ' + ver + ' 不存在', 'err'); return; }
      await updateDsh(ver);
    } catch (e) {
      setDshStatus('版本校验失败：' + (e.message || e), 'err');
    } finally {
      btnInstallVersion.disabled = false;
    }
  });
  dshVersionInput.addEventListener('keydown', (e) => { if (e.key === 'Enter') btnInstallVersion.click(); });

  $('btnCheckUpdate').addEventListener('click', () => refreshDsh());
  btnUpdateLatest.addEventListener('click', () => { if (dshLatest) updateDsh(dshLatest); });

  async function refreshRegistry() {
    try {
      const st = await invoke('get_shell_state');
      regInput.value = st.registry || 'https://registry.npmmirror.com';
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
        if (p.installed) {
          const btn = document.createElement('button');
          btn.className = 'sm danger-ghost';
          btn.textContent = '卸载';
          btn.addEventListener('click', async () => {
            // 二次确认：第一次点击变「确认卸载」，3 秒未再点自动还原；再点才执行
            if (btn.textContent !== '确认卸载') {
              btn.textContent = '确认卸载';
              const t0 = Date.now();
              const timer = setInterval(() => {
                if (btn.textContent === '确认卸载' && Date.now() - t0 >= 3000) {
                  btn.textContent = '卸载';
                  clearInterval(timer);
                }
              }, 500);
              return;
            }
            btn.disabled = true;
            try {
              const text = await invoke('plugin_op', { op: 'remove', pkg: p.name });
              pluginOutput.hidden = false;
              pluginOutput.textContent = text;
              setPluginStatus('已完成，工作台正在重启…', 'ok');
            } catch (e) {
              const msg = typeof e === 'string' ? e : (e && e.message) || String(e);
              pluginOutput.textContent = msg;
              setPluginStatus('卸载失败，详见下方输出', 'err');
            } finally {
              refreshPlugins(); // 重渲后按钮状态自然复位
            }
          });
          row.appendChild(btn);
        }
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

  // 关于页「项目主页」链接 → 系统默认浏览器
  $('aboutRepo').addEventListener('click', (e) => {
    e.preventDefault();
    invoke('open_repo_cmd').catch((e) => setAppStatus('打开项目主页失败：' + (e.message || e), 'err'));
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

  // 主菜单「关于」经 shell:tab 事件切 Tab（emit 方：main.rs menu-about）
})();
