// 检查（并可修补）exe 资源表里 DataEntry.OffsetToData 的取值。
//
// 【现在通常不需要 --write】
// 资源对象（.ci/make_rsrc_obj.js 生成）已经带了真正的
// IMAGE_REL_AMD64_ADDR32NB 重定位，链接器会自己把这些字段填成正确的
// 全局 RVA。所以这个脚本主要用来**校验**：干跑时如果每一项都显示
// "✅ 已是全局 RVA"，说明产物是好的。
//
// --write 保留给老式产物（不加重定位、链接后手工补 RVA）兜底用。
// 严格校验资源目录结构，任何可疑情况立即停止，避免把非资源数据
// 当成目录项去改（那会把整个 exe 改坏）。
//
// 用法：
//   node patch_rsrc_rva.js <exe>          干跑，只报告
//   node patch_rsrc_rva.js <exe> --write  实际写入
const fs = require('fs');

const path = process.argv[2];
const doWrite = process.argv.includes('--write');
if (!path) {
  console.error('用法: node patch_rsrc_rva.js <exe> [--write]');
  process.exit(1);
}

const b = fs.readFileSync(path);
const peOff = b.readUInt32LE(0x3c);
if (b.readUInt32LE(peOff) !== 0x00004550) { console.error('不是 PE'); process.exit(2); }
const coff = peOff + 4;
const nsec = b.readUInt16LE(coff + 2);
const optsz = b.readUInt16LE(coff + 16);
const optHdr = coff + 20;
const magic = b.readUInt16LE(optHdr);
const ddOff = optHdr + (magic === 0x20b ? 112 : 96);
const resRVA = b.readUInt32LE(ddOff + 16);
const resSize = b.readUInt32LE(ddOff + 20);
if (!resRVA || !resSize) { console.log('无资源目录'); process.exit(0); }

const secOff = optHdr + optsz;
let rsrcVA = 0, rsrcRaw = 0, rsrcVSize = 0;
for (let i = 0; i < nsec; i++) {
  const o = secOff + i * 40;
  const nm = b.slice(o, o + 8).toString('ascii').replace(/\0/g, '');
  if (nm === '.rsrc') {
    rsrcVSize = b.readUInt32LE(o + 8);
    rsrcVA = b.readUInt32LE(o + 12);
    rsrcRaw = b.readUInt32LE(o + 20);
    break;
  }
}
if (!rsrcVA) { console.error('.rsrc 不存在'); process.exit(3); }

const base = rsrcRaw + (resRVA - rsrcVA);
// 资源区可用长度（取 section 虚拟大小与剩余文件长度的小者）
const avail = Math.min(rsrcVSize, b.length - base);
console.log(`.rsrc VA=0x${rsrcVA.toString(16)} raw=0x${rsrcRaw.toString(16)} vsize=${rsrcVSize}`);
console.log(`资源目录 @ 文件偏移 0x${base.toString(16)}，可用 ${avail} 字节`);
console.log('');

// ---- 严格遍历：只走三层（type → name → language）----
const dataEntries = [];
const visited = new Set();

function readDir(off, depth, pathLabel) {
  if (depth > 3) return;
  if (visited.has(off)) return;
  visited.add(off);
  if (off < 0 || off + 16 > avail) {
    throw new Error(`目录头越界: off=${off} depth=${depth} (${pathLabel})`);
  }
  const abs = base + off;
  const named = b.readUInt16LE(abs + 12);
  const idc = b.readUInt16LE(abs + 14);
  const total = named + idc;
  if (total > 64) {
    // 资源目录的条目数不可能这么多，说明走错了地方
    throw new Error(`条目数异常: ${total} at off=${off} (${pathLabel})`);
  }
  if (off + 16 + total * 8 > avail) {
    throw new Error(`目录项越界: off=${off} total=${total} (${pathLabel})`);
  }

  for (let i = 0; i < total; i++) {
    const e = abs + 16 + i * 8;
    const nameField = b.readUInt32LE(e);
    const targetField = b.readUInt32LE(e + 4);
    const isSub = (targetField >>> 31) === 1;
    const toff = targetField & 0x7fffffff;

    let label;
    if ((nameField >>> 31) === 1) {
      // 字符串名：RVA 指向 2 字节长度 + UTF16
      const noff = base + (nameField & 0x7fffffff);
      if (noff + 2 <= b.length) {
        const len = b.readUInt16LE(noff);
        label = len > 0 && len < 200
          ? b.slice(noff + 2, noff + 2 + len * 2).toString('utf16le')
          : '<bad>';
      } else label = '<oob>';
    } else {
      label = String(nameField);
    }

    if (isSub) {
      readDir(toff, depth + 1, pathLabel + '/' + label);
    } else {
      if (toff + 16 > avail) {
        throw new Error(`数据项越界: toff=${toff} (${pathLabel}/${label})`);
      }
      const dabs = base + toff;
      const dataRVA = b.readUInt32LE(dabs);
      const dataSize = b.readUInt32LE(dabs + 4);
      if (dataSize > avail) {
        throw new Error(`数据大小异常: ${dataSize} (${pathLabel}/${label})`);
      }
      dataEntries.push({
        entryOff: toff,
        abs: dabs,
        cur: dataRVA,
        size: dataSize,
        label: pathLabel + '/' + label,
      });
    }
  }
}

try {
  readDir(0, 0, '');
} catch (e) {
  console.error('\n❌ 结构校验失败，已中止（未修改文件）：');
  console.error('   ' + e.message);
  process.exit(4);
}

console.log(`发现 ${dataEntries.length} 个数据项：`);
let needFix = 0;
for (const d of dataEntries) {
  const ok = d.cur >= rsrcVA && d.cur < rsrcVA + rsrcVSize;
  const newRva = rsrcVA + d.cur;
  console.log(
    `  ${d.label.padEnd(28)} off=${String(d.entryOff).padStart(5)} ` +
    `rva=0x${d.cur.toString(16).padStart(6)} size=${String(d.size).padStart(6)} ` +
    (ok ? '✅ 已是全局 RVA' : `→ 应改为 0x${newRva.toString(16)}`)
  );
  if (!ok) needFix++;
}

console.log('');
if (needFix === 0) {
  console.log('无需修补。');
  process.exit(0);
}
if (!doWrite) {
  console.log(`需要修补 ${needFix} 项。加 --write 实际写入。`);
  process.exit(0);
}

for (const d of dataEntries) {
  const ok = d.cur >= rsrcVA && d.cur < rsrcVA + rsrcVSize;
  if (!ok) {
    b.writeUInt32LE(rsrcVA + d.cur, d.abs);
  }
}
fs.writeFileSync(path, b);
console.log(`✅ 已修补 ${needFix} 项并写入 ${path}`);
