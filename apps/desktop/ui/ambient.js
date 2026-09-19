/* 氛围背景鼠标交互 —— 方案 7「视差分层 + 邻近点亮」（设计稿选定）
 *
 * 设计约束（用户明确要求）：**不改任何排版**。因此这里只做「叠加」：
 *   · 不动 theme.css 里 .ambient / .ambient-info / .ambient-mark 的任何位置/尺寸/字号；
 *   · 交互时仅给已有元素写 transform（位移 ≤26px，鼠标移出立即回位到 0）；
 *   · 新增的装饰层（点亮网格 .ambient-pulse / 涟漪 .ambient-ripple / 点亮副本 .ambient-lit）
 *     都是绝对定位、pointer-events:none、脱离文档流，不参与布局；
 *   · 点亮文字用「整块副本 + 移动径向遮罩」而不是把原文拆成逐字符 span——拆分会被迫打断
 *     文本整段 shaping（实测字宽 +0.16px、居中位置漂移 0.01px），属于改动排版，已弃用。
 *
 * 开销：只在浮层打开（body.overlay-open ⇒ 背景真正露出）时挂监听，平时零开销；
 * mousemove 用 rAF 合并；基准位置只在浮层开关 / 抽屉开关 / 尺寸变化 / 字体就绪时重算。
 * prefers-reduced-motion 或页面隐藏时完全不启用。
 */
(function () {
  'use strict';
  try {
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;

    var body = document.body;
    var num = function (v, d) { var n = parseFloat(v); return isFinite(n) ? n : d; };
    var clamp = function (v, a, b) { return v < a ? a : v > b ? b : v; };

    var amb = null, grid = null, mark = null, info = null, glowA = null, glowB = null;
    var pulse = null, ripple = null, lit = null;
    // 启动加载页（#startupView）：同一套交互也覆盖它——图标做视差+靠近发亮，标题做「副本+遮罩」点亮
    var stEl = null, stMark = null, stTitle = null, stLit = null, stWatch = false;
    var markGlow = null, stGlow = null;   // 光晕层：替代图标上的 filter:drop-shadow（滤镜离屏缓冲会被裁切，看起来像方块）
    var sweepTimer = 0, sweepLast = 0, lastMoveAt = 0;
    var active = false, raf = 0, pending = null, last = null;
    var lastRipple = { x: 0, y: 0 }, drawerW = 560, wasOpen = false, wasDrawer = false;
    var cloning = false, classTimer = 0;


    /* 交互期才挂载的类：带 will-change/transition。静止时（含浮层打开但没动鼠标）
       完全不存在——否则模糊光晕/遮罩网格会走另一条栅格化路径，静止渲染与改造前不再
       逐像素一致（实测静止态 9.7% 像素有微差，加上「用完即摘」后归零）。 */
    function needParallaxClass() {
      if (amb && !amb.classList.contains('ambient-parallax')) amb.classList.add('ambient-parallax');
    }
    function dropParallaxClassSoon() {
      clearTimeout(classTimer);
      classTimer = setTimeout(function () {
        if (amb && !last) amb.classList.remove('ambient-parallax');
      }, 340); // 等回位过渡（110/150ms）播完再摘
    }

    /* 点亮副本：内容与信息块逐字一致（去掉 id，避免重复 id），仅作遮罩显影用 */
    function refreshLit() {
      if (!info || !lit) return;
      cloning = true;
      var src = info.cloneNode(true);
      src.removeAttribute('id');
      var all = src.querySelectorAll('[id]');
      for (var i = 0; i < all.length; i++) all[i].removeAttribute('id');
      src.setAttribute('aria-hidden', 'true');
      lit.innerHTML = '';
      while (src.firstChild) lit.appendChild(src.firstChild);
      cloning = false;
    }

    function build() {
      amb = document.querySelector('.ambient');
      if (!amb) return false;
      grid = amb.querySelector('.ambient-grid');
      mark = amb.querySelector('.ambient-mark');
      info = amb.querySelector('.ambient-info');
      glowA = amb.querySelector('.ambient-glow--a');
      glowB = amb.querySelector('.ambient-glow--b');
      if (!grid || !mark || !info || !glowA || !glowB) return false;
      if (!pulse) {
        pulse = document.createElement('div');
        pulse.className = 'ambient-pulse';
        amb.insertBefore(pulse, mark); // 在图标之下：图标始终是主视觉
      }
      if (!lit) {
        lit = document.createElement('div');
        lit.className = 'ambient-info ambient-lit'; // 复用 .ambient-info 的排版，天然与原文重合
        lit.setAttribute('aria-hidden', 'true');
        info.parentNode.insertBefore(lit, info.nextSibling);
      }
      if (!markGlow) {
        markGlow = document.createElement('div');
        markGlow.className = 'ambient-mark-glow';
        amb.insertBefore(markGlow, mark); // 在水印之下：光晕不参与任何滤镜/分层
      }
      if (!ripple) {
        ripple = document.createElement('div');
        ripple.className = 'ambient-ripple';
        amb.appendChild(ripple);
      }
      refreshLit();
      // 启动加载页元素（可能不存在：打包运行早期/隐藏态）
      stEl = document.getElementById('startupView');
      if (stEl) {
        stMark = stEl.querySelector('.startup-mark');
        stTitle = stEl.querySelector('.startup-title');
        if (stTitle && !stLit) {
          stLit = document.createElement('div');
          stLit.className = 'startup-title startup-lit';
          stLit.setAttribute('aria-hidden', 'true');
          stLit.textContent = stTitle.textContent; // 纯文本副本：不动原文一个字
          stEl.appendChild(stLit);
        } else if (stTitle && stLit) {
          stLit.textContent = stTitle.textContent; // 文案更新时跟随
        }
        if (!stEl.querySelector('.startup-breath')) {
          var br = document.createElement('div');
          br.className = 'startup-breath';
          br.setAttribute('aria-hidden', 'true');
          stEl.insertBefore(br, stEl.firstChild); // 画在内容之后
        }
        if (!stGlow && stEl) {
          stGlow = document.createElement('div');
          stGlow.className = 'startup-mark-glow';
          stEl.appendChild(stGlow); // 绝对定位、脱离文档流，不影响加载页排版
        }
        if (!stWatch) {
          stWatch = true;
          // 启动页由 shell.js 切 hidden/style 隐藏：属性变化需重新同步（决定是否继续挂监听）
          new MutationObserver(sync).observe(stEl, { attributes: true, attributeFilter: ['hidden', 'style', 'class'] });
        }
      }
      // 信息块内容会随状态更新（地址/版本）：跟随刷新副本；按标志位防自触发
      if (!info.dataset.ambWatch) {
        info.dataset.ambWatch = '1';
        new MutationObserver(function () { if (!cloning) refreshLit(); })
          .observe(info, { childList: true, subtree: true, characterData: true });
      }
      return true;
    }

    /* 基准位置：在「无位移」状态下量取（collect 前先 onLeave 复位） */
    function collect() {
      if (!amb || !info || !mark) return;
      if (markGlow && mark) {
        var gsz = Math.round(Math.max(220, mark.getBoundingClientRect().width * 1.7));
        markGlow.style.width = markGlow.style.height = gsz + 'px';
        markGlow.style.margin = (-gsz / 2) + 'px 0 0 ' + (-gsz / 2) + 'px';
      }
      if (stGlow && stMark && startupVisible() && stMark.getBoundingClientRect().width > 0) {
        var sgs = Math.round(stMark.getBoundingClientRect().width * 3);
        stGlow.style.width = stGlow.style.height = sgs + 'px';
        stGlow.style.margin = (-sgs / 2) + 'px 0 0 ' + (-sgs / 2) + 'px';
      }
    }

    /* 启动加载页是否正在显示（shell.js 用 hidden + style.display 双保险切换） */
    function startupVisible() {
      if (!stEl) stEl = document.getElementById('startupView'); // 惰性查找：首次 sync 时 build 还没跑
      if (!stEl) return false;
      if (stEl.hidden || stEl.style.display === 'none') return false;
      var r = stEl.getBoundingClientRect();
      return r.width > 0 && r.height > 0;
    }

    /* 可见区域宽度：抽屉打开时右侧被占用，位移/点亮都以「剩余区域」为中心 */
    function visibleWidth() {
      if (!body.classList.contains('drawer-open')) return window.innerWidth;
      var css = getComputedStyle(document.documentElement);
      drawerW = num(css.getPropertyValue('--drawer-w'), num(css.getPropertyValue('--dsh-drawer-w'), 560));
      return Math.max(1, window.innerWidth - drawerW);
    }

    function apply(x, y, speed) {
      var w = visibleWidth(), h = window.innerHeight;
      var px = clamp((x / w - 0.5) * 2, -1, 1);
      var py = clamp((y / h - 0.5) * 2, -1, 1);
      // 「让位」判定：鼠标落在任何浮层（抽屉 / 命令面板 / 顶栏）上时收敛效果，
      // 不与面板抢注意力。抽屉另有几何判定（右侧预留宽度），这里再加命中判定兜底。
      var dim = false;
      if (body.classList.contains('drawer-open') && x > w) dim = true;
      if (!dim) {
        var under = document.elementFromPoint(x, y);
        if (under && under.closest && under.closest('.drawer, .palette, .palette-overlay, .chrome')) dim = true;
      }
      var f = dim ? 0.35 : 1;
      var fx = function (v) { return v.toFixed(1) + 'px'; };
      var iox = -px * 10 * f, ioy = -py * 7 * f;

      // ① 视差分层（深度越小位移越大；与设计稿同参数）
      glowA.style.transform = 'translate(' + fx(-px * 26 * f) + ',' + fx(-py * 18 * f) + ')';
      glowB.style.transform = 'translate(' + fx(-px * 18 * f) + ',' + fx(-py * 13 * f) + ')';
      var gt = 'translate(' + fx(-px * 12 * f) + ',' + fx(-py * 8 * f) + ')';
      grid.style.transform = gt;
      pulse.style.transform = gt; // 与底色网格同步位移：点亮区的线与底色线不会错位
      mark.style.transform = 'translate(-50%,-50%) translate(' + fx(-px * 18 * f) + ',' + fx(-py * 12 * f) + ')';
      var it = 'translate(-50%,-50%) translate(' + fx(iox) + ',' + fx(ioy) + ')';
      info.style.transform = it;
      if (lit) lit.style.transform = it; // 副本与原文同步位移，始终严丝合缝

      // ② 邻近点亮：网格成片提亮
      pulse.style.setProperty('--amb-x', fx(x));
      pulse.style.setProperty('--amb-y', fx(y));
      pulse.style.setProperty('--amb-r', clamp(150 + speed * 0.5, 150, 300).toFixed(0) + 'px');
      pulse.style.opacity = '1';

      // ② 邻近点亮：文字用副本的移动遮罩显影（坐标换算到副本自身的局部坐标系）
      if (lit) {
        // 实时矩形含自身 transform（translate(-50%,-50%) translate(iox,ioy)），而 mask 坐标用的是
        // **变换前**的局部坐标 ⇒ 必须把「宽/高的一半」和已知位移一起还原回去，否则光斑会偏半个元素。
        var li = lit.getBoundingClientRect();
        lit.style.setProperty('--amb-lx', fx(x - (li.left + li.width / 2 - iox)));
        lit.style.setProperty('--amb-ly', fx(y - (li.top + li.height / 2 - ioy)));
        lit.style.setProperty('--amb-lr', (dim ? 60 : clamp(105 + speed * 0.5, 105, 190)).toFixed(0) + 'px');
        lit.style.opacity = '1';
      }

      // 图标被"照亮"：用独立光晕层（无滤镜 ⇒ 不产生离屏缓冲/方块边缘）
      if (markGlow) {
        var mg = mark.getBoundingClientRect(); // 实时：水印会随 --drawer-w 重排
        var md = Math.hypot(mg.left + mg.width / 2 - x, mg.top + mg.height / 2 - y);
        var near2 = clamp(1 - md / 360, 0, 1) * (dim ? 0.4 : 1);
        markGlow.style.transform = 'translate(' + fx(mg.left + mg.width / 2) + ',' + fx(mg.top + mg.height / 2) + ')';
        markGlow.style.opacity = (near2 * 0.9).toFixed(3);
      }

      // 启动加载页：图标视差 + 靠近发亮、标题副本遮罩点亮（内容与排版不动）
      // 位置一律读实时矩形：抽屉拖拽会改 --drawer-w，标题/图标会重排，缓存矩形会过期（用户实测错位）
      if (stLit && stLit.parentNode) {
        var sEl = stEl.getBoundingClientRect();
        if (sEl.width > 0) { // 加载页不可见时整块跳过（浮层场景每帧省 3 次读 + 若干写）
          // 标题点亮（无标题则整条跳过，不影响下面图标效果）
          if (stTitle) {
            var ti = stTitle.getBoundingClientRect();
            stLit.style.left = (ti.left - sEl.left) + 'px';
            stLit.style.top = (ti.top - sEl.top) + 'px';
            stLit.style.width = ti.width + 'px';
            stLit.style.setProperty('--amb-lx', fx(x - ti.left)); // 副本无自身 transform ⇒ 直接实时矩形
            stLit.style.setProperty('--amb-ly', fx(y - ti.top));
            stLit.style.setProperty('--amb-lr', (dim ? 70 : clamp(120 + speed * 0.5, 120, 220)).toFixed(0) + 'px');
            stLit.style.opacity = '1';
          }
          // 图标：视差 + 光晕（与"是否有标题"解耦，结构变了也不会一起失效）
          if (stMark) {
            var mi = stMark.getBoundingClientRect();
            var near = clamp(1 - Math.hypot(mi.left + mi.width / 2 - x, mi.top + mi.height / 2 - y) / 300, 0, 1) * (dim ? 0.4 : 1);
            stMark.style.transform = 'translate(' + fx(-px * 10 * f) + ',' + fx(-py * 7 * f) + ')';
            if (stGlow) {
              stGlow.style.transform = 'translate(' + fx(mi.left - sEl.left + mi.width / 2) + ',' + fx(mi.top - sEl.top + mi.height / 2) + ')';
              stGlow.style.opacity = (near * 0.95).toFixed(3);
            }
          }
        }
      }

      // 涟漪：按行进距离触发（不平滑滚动时抖动）
      if (!dim && speed > 0 && Math.hypot(x - lastRipple.x, y - lastRipple.y) > 90) {
        lastRipple = { x: x, y: y };
        ripple.style.left = fx(x);
        ripple.style.top = fx(y);
        ripple.classList.remove('on');
        void ripple.offsetWidth; // 重启 CSS 动画
        ripple.classList.add('on');
      }
    }

    /* 加载页自动扫光：鼠标静止时，点亮位置沿缓慢轨迹自己游走（内容不变，纯装饰） */
    function sweep(ts) {
      sweepTimer = 0;
      if (!active || !startupVisible() || document.hidden) return;
      if (ts - sweepLast > 40) { // ~25fps 足够，省电
        sweepLast = ts;
        if (!last || ts - lastMoveAt > 1200) {
          var w = window.innerWidth, h = window.innerHeight;
          var vx = w * (0.5 + 0.33 * Math.sin(ts / 2600));
          var vy = h * (0.52 + 0.24 * Math.cos(ts / 3100));
          apply(vx, vy, 0);
        }
      }
      sweepTimer = requestAnimationFrame(sweep);
    }

    function onMove(e) {
      if (document.hidden) return;
      needParallaxClass();
      lastMoveAt = performance.now();
      var x = e.clientX, y = e.clientY;
      var speed = last ? Math.hypot(x - last.x, y - last.y) : 0;
      last = { x: x, y: y };
      pending = { x: x, y: y, speed: speed };
      if (raf) return;
      raf = requestAnimationFrame(function () {
        raf = 0;
        if (pending) { apply(pending.x, pending.y, pending.speed); pending = null; }
      });
    }

    function onLeave() {
      if (!amb) return;
      pending = null;
      if (raf) { cancelAnimationFrame(raf); raf = 0; }
      last = null;
      glowA.style.transform = glowB.style.transform = '';
      grid.style.transform = '';
      pulse.style.transform = '';
      pulse.style.opacity = '0';
      mark.style.transform = 'translate(-50%,-50%)';
      mark.style.filter = '';
      info.style.transform = 'translate(-50%,-50%)';
      if (lit) {
        lit.style.transform = 'translate(-50%,-50%)';
        lit.style.opacity = '0';
      }
      if (stLit) stLit.style.opacity = '0';
      if (stMark) stMark.style.transform = '';
      if (markGlow) markGlow.style.opacity = '0';
      if (stGlow) stGlow.style.opacity = '0';
      dropParallaxClassSoon();
    }

    function onResize() { if (active) { onLeave(); collect(); } }

    /* 窗口隐藏时收敛效果；恢复可见时若还在加载页则重启自动扫光（否则扫光会一直停着） */
    function onVisibility() {
      if (document.hidden) { onLeave(); return; }
      onLeave();
      collect();
      syncSweep();
    }

    function syncSweep() {
      var want = active && startupVisible() && !document.hidden && !sweepTimer;
      if (want) sweepTimer = requestAnimationFrame(sweep);
    }

    function sync() {
      var open = body.classList.contains('overlay-open');
      var drawer = body.classList.contains('drawer-open');
      if ((open || startupVisible()) && build()) {
        if (!wasOpen) {
          onLeave();
          collect();
          window.addEventListener('mousemove', onMove, { passive: true });
          window.addEventListener('mouseleave', onLeave);
          window.addEventListener('resize', onResize);
          document.addEventListener('visibilitychange', onVisibility);
        } else if (drawer !== wasDrawer) {
          onLeave();
          collect();
        }
      } else if (wasOpen) {
        window.removeEventListener('mousemove', onMove);
        window.removeEventListener('mouseleave', onLeave);
        window.removeEventListener('resize', onResize);
        document.removeEventListener('visibilitychange', onVisibility);
        clearTimeout(classTimer);
        if (amb) amb.classList.remove('ambient-parallax');
        onLeave();
      }
      wasOpen = open;
      wasDrawer = drawer;
      active = open;
      syncSweep();
    }

    new MutationObserver(sync).observe(body, { attributes: true, attributeFilter: ['class'] });
    sync();
    if (document.fonts && document.fonts.ready) {
      document.fonts.ready.then(function () { if (active) { onLeave(); collect(); } });
    }
    window.addEventListener('load', function () { if (active) { onLeave(); collect(); } });
  } catch (_) { /* 纯装饰层：任何异常都不影响壳页功能 */ }
})();
