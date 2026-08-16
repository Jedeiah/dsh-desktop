// 插件管理：调用 Rust 侧 plugin_op 命令（安装/卸载），实时流式回显输出。
// Rust 侧逐行 emit "plugin-output" 事件；invoke 返回后以最终输出（含退出码）
// 替换实时累积，保证完整性。
(() => {
  'use strict';
  const pkg = document.getElementById('pkg');
  const btnInstall = document.getElementById('btnInstall');
  const btnRemove = document.getElementById('btnRemove');
  const status = document.getElementById('status');
  const output = document.getElementById('output');

  let unlisten = null;

  function setBusy(busy) {
    btnInstall.disabled = busy;
    btnRemove.disabled = busy;
    pkg.disabled = busy;
  }

  function setStatus(text, kind) {
    status.textContent = text;
    status.className = 'status ' + (kind || '');
  }

  // 实时输出流：Rust 侧 run_dsh_plugin 每读一行 emit 一次。
  // Tauri 环境缺失（如直接 file:// 打开）或 listen 失败时静默降级：
  // 无实时流，仅保留最终输出（invoke 返回后展示）。
  try {
    window.__TAURI__.event.listen('dsh:plugin-output', (e) => {
      output.hidden = false;
      output.textContent += e.payload + '\n';
      output.scrollTop = output.scrollHeight;
    }).then((fn) => { unlisten = fn; }).catch(() => { /* 能力缺失：降级为仅最终输出 */ });
  } catch (e) {
    /* 降级：无实时流 */
  }

  async function run(op) {
    const name = pkg.value.trim();
    if (!name) {
      setStatus('请输入包名', 'err');
      return;
    }
    setBusy(true);
    setStatus(op === 'add' ? '正在安装…（首次需下载依赖，可能较久）' : '正在卸载…');
    output.hidden = false;
    output.textContent = '';
    try {
      const text = await window.__TAURI__.core.invoke('plugin_op', { op, pkg: name });
      // 最终输出（含退出码）替换实时累积，避免重复
      output.textContent = text;
      setStatus(op === 'add' ? '安装完成，请重启工作台生效' : '卸载完成，请重启工作台生效', 'ok');
    } catch (e) {
      const msg = typeof e === 'string' ? e : (e && e.message) || String(e);
      output.textContent = msg;
      setStatus('操作失败，详见下方输出', 'err');
    } finally {
      setBusy(false);
    }
  }

  btnInstall.addEventListener('click', () => run('add'));
  btnRemove.addEventListener('click', () => run('remove'));
  pkg.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') run('add');
  });
  window.addEventListener('beforeunload', () => {
    if (unlisten) unlisten();
  });
})();
