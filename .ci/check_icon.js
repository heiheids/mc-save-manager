// 独立校验 exe 的图标资源是否完整可用（不依赖 PowerShell / System.Drawing）。
//
// 检查三件事：
//   1. .rsrc 节存在
//   2. 资源目录里能找到 RT_GROUP_ICON (14) 和 RT_ICON (3)
//   3. 每个 ICON 数据项的 OffsetToData 是**全局 RVA**（落在 .rsrc 区间内），
//      而不是残留的节内偏移
//
// 用法：node check_icon.js <exe>
const fs = require('fs');

const path = process.argv[2];
if (!path) {
  console.error('用法: node check_icon.js <exe>');
  process.exit(2);
}

const b = fs.readFileSync(path);
if (b.readUInt16LE(0) !== 0x5A4D) {
  console.error('不是 PE 文件（缺 MZ）');
  process.exit(1);
}

const pe = b.readUInt32LE(0x3C);
if (b.readUInt32LE(pe) !== 0x00004550) {
  console.error('不是 PE 文件（缺 PE\\0\\0）');
  process.exit(1);
}

const nsec = b.readUInt16LE(pe + 6);
const optSize = b.readUInt16LE(pe + 20);
const secOff = pe + 24 + optSize;

let rsrc = null;
const secs = [];
for (let i = 0; i < nsec; i++) {
  const o = secOff + i * 40;
  const name = b.toString('ascii', o, o + 8).replace(/\0/g, '').trim();
  const vsize = b.readUInt32LE(o + 8);
  const vaddr = b.readUInt32LE(o + 12);
  const rawSize = b.readUInt32LE(o + 16);
  const rawPtr = b.readUInt32LE(o + 20);
  secs.push(name);
  if (name === '.rsrc') rsrc = { vsize, vaddr, rawSize, rawPtr };
}

console.log('节: ' + secs.join(', '));
if (!rsrc) {
  console.log('❌ 缺少 .rsrc 节 —— 这个 exe 没有图标资源');
  process.exit(1);
}
console.log(`.rsrc  VA=0x${rsrc.vaddr.toString(16)}  size=${rsrc.rawSize}`);

// 解析资源目录树
const base = rsrc.rawPtr;
const rvaMin = rsrc.vaddr;
const rvaMax = rsrc.vaddr + rsrc.rawSize;

function readDir(off) {
  const namedCount = b.readUInt16LE(base + off + 12);
  const idCount = b.readUInt16LE(base + off + 14);
  const out = [];
  for (let i = 0; i < namedCount + idCount; i++) {
    const e = base + off + 16 + i * 8;
    const nameField = b.readUInt32LE(e);
    const offField = b.readUInt32LE(e + 4);
    out.push({
      id: nameField & 0x80000000 ? null : nameField,
      isDir: (offField & 0x80000000) !== 0,
      offset: offField & 0x7fffffff,
    });
  }
  return out;
}

const root = readDir(0);
const types = root.map((r) => r.id).filter((x) => x !== null);
console.log('顶层资源类型: ' + types.map((t) => `${t}`).join(', '));

const hasGroup = types.includes(14);
const hasIcon = types.includes(3);
console.log(`RT_ICON(3): ${hasIcon ? '✅' : '❌'}   RT_GROUP_ICON(14): ${hasGroup ? '✅' : '❌'}`);

// 检查所有数据项的 RVA 是否正确（全局 RVA vs 节内偏移）
let total = 0;
let bad = 0;
function walk(off, depth) {
  const entries = readDir(off);
  for (const e of entries) {
    if (e.isDir) {
      if (depth < 2) walk(e.offset, depth + 1);
    } else {
      const dataEntry = base + e.offset;
      const dataRva = b.readUInt32LE(dataEntry);
      const size = b.readUInt32LE(dataEntry + 4);
      total++;
      const ok = dataRva >= rvaMin && dataRva < rvaMax;
      if (!ok) {
        bad++;
        console.log(
          `  ⚠ 数据项 RVA=0x${dataRva.toString(16)} 不在 .rsrc 区间 [0x${rvaMin.toString(16)}, 0x${rvaMax.toString(16)}) size=${size}`
        );
      }
    }
  }
}
walk(0, 0);

console.log(`\n数据项共 ${total} 个，RVA 异常 ${bad} 个`);
if (bad === 0 && hasIcon && hasGroup) {
  console.log('✅ 图标资源完整，Windows 应能正常显示');
  process.exit(0);
} else {
  console.log('❌ 图标资源有问题');
  process.exit(1);
}
