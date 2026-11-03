// 从导出的 HTML 里抽出 <script> 内容做语法检查（不执行，只 parse）。
const fs = require('fs');
const vm = require('vm');

const html = fs.readFileSync(process.argv[2], 'utf8');
const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => m[1]);

console.log(`找到 ${scripts.length} 段 <script>`);

let ok = 0;
scripts.forEach((src, i) => {
  try {
    new vm.Script(src);
    const lines = src.split('\n').length;
    console.log(`  [${i}] 语法 OK，${lines} 行`);
    ok++;
  } catch (e) {
    console.log(`  [${i}] ❌ 语法错误: ${e.message}`);
  }
});

// 顺带检查关键 DOM id 是否都存在于 HTML 里
const ids = ['cursor', 'gridToggle'];
for (const id of ids) {
  const has = html.includes(`id="${id}"`);
  console.log(`  id="${id}": ${has ? '✅' : '❌ 缺失'}`);
}

// 检查 CSS 里是否引用了不存在的 class
for (const cls of ['dragging', 'grid on', 'pane active']) {
  console.log(`  class "${cls}": ${html.includes(cls.split(' ')[0]) ? '✅' : '⚠ 未引用'}`);
}

process.exit(ok === scripts.length ? 0 : 1);
