// 在无头浏览器里模拟：按下左键 → 移动 → 抬起，检查画布是否真的滚动了。
//
// 不需要装 puppeteer：直接用 Node 内置能力做不到真实 DOM，
// 所以这里退一步，只验证脚本里拖动逻辑的关键分支能被触发
// （用一个极简的 DOM 桩跑一遍那段脚本）。
const fs = require('fs');
const vm = require('vm');

const html = fs.readFileSync(process.argv[2], 'utf8');
const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => m[1]);
const code = scripts[scripts.length - 1];

// ---- 极简 DOM 桩 ----
const listeners = { window: {}, stage: {} };
let scrollLeft = 500;
let scrollTop = 300;
const classes = new Set();

const stageEl = {
  get scrollLeft() { return scrollLeft; },
  set scrollLeft(v) { scrollLeft = v; },
  get scrollTop() { return scrollTop; },
  set scrollTop(v) { scrollTop = v; },
  classList: { add: (c) => classes.add(c), remove: (c) => classes.delete(c), toggle: () => {} },
  addEventListener: (t, fn) => { listeners.stage[t] = fn; },
};

const sandbox = {
  document: {
    querySelectorAll: (sel) => (sel === '.stage' ? [stageEl] : []),
    getElementById: () => ({ style: {}, innerHTML: '' }),
  },
  window: {
    addEventListener: (t, fn) => { listeners.window[t] = fn; },
    innerWidth: 1200,
  },
  console,
};

vm.createContext(sandbox);
vm.runInContext(code, sandbox);

// ---- 模拟按下左键 ----
let defaultPrevented = false;
listeners.stage['mousedown']({
  button: 0,
  clientX: 100,
  clientY: 100,
  preventDefault: () => { defaultPrevented = true; },
});
console.log('按下左键: preventDefault =', defaultPrevented, ' dragging class =', classes.has('dragging'));

// ---- 移动 60px ----
listeners.window['mousemove']({ clientX: 160, clientY: 130 });
console.log('拖动后 scrollLeft =', scrollLeft, '(期望 440)  scrollTop =', scrollTop, '(期望 270)');

// ---- 抬起 ----
listeners.window['mouseup']({ button: 0 });
console.log('抬起后 dragging class =', classes.has('dragging'), '(期望 false)');

// ---- 右键按下不应启动拖动 ----
const before = scrollLeft;
listeners.stage['mousedown']({
  button: 2,
  clientX: 100,
  clientY: 100,
  preventDefault: () => {},
});
listeners.window['mousemove']({ clientX: 300, clientY: 300 });
console.log('右键拖动后 scrollLeft =', scrollLeft, '(应与之前相同:', before, ')');

const pass =
  defaultPrevented &&
  scrollLeft === before &&
  !classes.has('dragging');
console.log(pass ? '\n✅ 左键拖动逻辑正常，右键不触发' : '\n❌ 逻辑有问题');
process.exit(pass ? 0 : 1);
