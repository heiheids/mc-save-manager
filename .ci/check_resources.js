// 列出 exe 的全部资源类型，判断"像不像一个正常 Windows 程序"
// 正常程序通常有：RT_ICON(3) / RT_GROUP_ICON(14) / RT_MANIFEST(24) / RT_VERSION(16)
const fs = require('fs');

const path = process.argv[2];
const b = fs.readFileSync(path);
const pe = b.readUInt32LE(0x3C);
const nsec = b.readUInt16LE(pe + 6);
const optSize = b.readUInt16LE(pe + 20);
const secOff = pe + 24 + optSize;

let rsrc = null;
const secs = [];
for (let i = 0; i < nsec; i++) {
  const o = secOff + i * 40;
  const name = b.toString('ascii', o, o + 8).replace(/\0/g, '').trim();
  secs.push(name);
  if (name === '.rsrc') {
    rsrc = {
      vaddr: b.readUInt32LE(o + 12),
      rawSize: b.readUInt32LE(o + 16),
      rawPtr: b.readUInt32LE(o + 20),
    };
  }
}
console.log('文件: ' + path);
console.log('节: ' + secs.join(', '));

const NAMES = {
  1: 'RT_CURSOR', 2: 'RT_BITMAP', 3: 'RT_ICON', 4: 'RT_MENU',
  5: 'RT_DIALOG', 6: 'RT_STRING', 7: 'RT_FONTDIR', 8: 'RT_FONT',
  9: 'RT_ACCELERATOR', 10: 'RT_RCDATA', 11: 'RT_MESSAGETABLE',
  12: 'RT_GROUP_CURSOR', 14: 'RT_GROUP_ICON', 16: 'RT_VERSION',
  24: 'RT_MANIFEST',
};

if (!rsrc) {
  console.log('❌ 没有 .rsrc（无图标、无版本信息、无 manifest）');
  process.exit(0);
}

const base = rsrc.rawPtr;
const named = b.readUInt16LE(base + 12);
const ids = b.readUInt16LE(base + 14);
const types = [];
for (let i = 0; i < named + ids; i++) {
  const e = base + 16 + i * 8;
  const nameField = b.readUInt32LE(e);
  if (!(nameField & 0x80000000)) types.push(nameField);
}

console.log('资源类型:');
for (const t of types) {
  console.log(`  ${t}  ${NAMES[t] || '(未知)'}`);
}

const need = [[3, 'RT_ICON'], [14, 'RT_GROUP_ICON'], [16, 'RT_VERSION'], [24, 'RT_MANIFEST']];
console.log('\n正常 Windows 程序应当具备:');
for (const [id, name] of need) {
  console.log(`  ${name.padEnd(16)} ${types.includes(id) ? '✅ 有' : '❌ 缺失'}`);
}
