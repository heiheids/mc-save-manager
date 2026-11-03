// 从 exe 的 RT_VERSION 资源里把字符串读出来（不依赖 PowerShell）。
const fs = require('fs');

const path = process.argv[2];
const b = fs.readFileSync(path);
const pe = b.readUInt32LE(0x3c);
const nsec = b.readUInt16LE(pe + 6);
const optSize = b.readUInt16LE(pe + 20);
const secOff = pe + 24 + optSize;

let rsrc = null;
for (let i = 0; i < nsec; i++) {
  const o = secOff + i * 40;
  if (b.toString('ascii', o, o + 8).replace(/\0/g, '').trim() === '.rsrc') {
    rsrc = { vaddr: b.readUInt32LE(o + 12), rawSize: b.readUInt32LE(o + 16), rawPtr: b.readUInt32LE(o + 20) };
  }
}
if (!rsrc) { console.log('无 .rsrc'); process.exit(1); }

const base = rsrc.rawPtr;

function entries(off) {
  const nNamed = b.readUInt16LE(base + off + 12);
  const nId = b.readUInt16LE(base + off + 14);
  const out = [];
  for (let i = 0; i < nNamed + nId; i++) {
    const e = base + off + 16 + i * 8;
    out.push({
      id: b.readUInt32LE(e),
      isDir: (b.readUInt32LE(e + 4) & 0x80000000) !== 0,
      off: b.readUInt32LE(e + 4) & 0x7fffffff,
    });
  }
  return out;
}

// 找 RT_VERSION (16)
const typeEntry = entries(0).find((x) => x.id === 16);
if (!typeEntry) { console.log('无 RT_VERSION'); process.exit(1); }
const nameEntry = entries(typeEntry.off).find((x) => x.isDir);
const leaf = entries(nameEntry.off)[0];
const dataRva = b.readUInt32LE(base + leaf.off);
const size = b.readUInt32LE(base + leaf.off + 4);
const fileOff = rsrc.rawPtr + (dataRva - rsrc.vaddr);

console.log(`RT_VERSION: ${size} 字节 @ file 0x${fileOff.toString(16)}`);

// VS_FIXEDFILEINFO 在 VS_VERSION_INFO 的 Value 处。这里直接扫 UTF-16 字符串。
const raw = b.subarray(fileOff, fileOff + size);
const str = raw.toString('utf16le');
const parts = str.split('\0').filter((s) => s.trim().length > 0);
console.log('内嵌字符串:');
for (const p of parts) {
  console.log('  ' + p);
}
