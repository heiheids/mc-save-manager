// 生成链接用的 COFF 目标文件，内含完整的 .rsrc 资源节：
//
//   RT_ICON        (3)  应用图标各尺寸
//   RT_GROUP_ICON  (14) 图标组
//   RT_VERSION     (16) 版本信息   ← 正常 Windows 程序都有
//   RT_MANIFEST    (24) 清单       ← 声明 DPI 感知与执行级别
//
// 为什么必须要有 RT_VERSION / RT_MANIFEST：
//   早期版本只塞了图标。一个"没有任何版本信息、也没有清单"的 exe，
//   在杀毒软件的启发式里就是"来路不明的自制程序"，很容易被报毒或者
//   被 SmartScreen 拦下。补齐这两项之后它才像一个正常的 Windows 应用。
//
//   附带好处：清单里声明 dpiAware 之后，高 DPI 屏幕上界面不再发虚。
//
// 用法：node make_rsrc_obj.js <icon.ico> <out.obj> [version]

const fs = require('fs');
const path = require('path');

const [, , icoPath, outPath, verArg] = process.argv;
if (!icoPath || !outPath) {
  console.error('用法: node make_rsrc_obj.js <icon.ico> <out.obj> [version]');
  process.exit(2);
}

// 版本号：命令行参数 > Cargo.toml
let version = verArg;
if (!version) {
  try {
    const cargo = fs.readFileSync(path.join(__dirname, '..', 'Cargo.toml'), 'utf8');
    const m = cargo.match(/^version\s*=\s*"([^"]+)"/m);
    if (m) version = m[1];
  } catch {
    /* 忽略 */
  }
}
version = version || '1.0.0';

const parts = version.split('.').map((x) => parseInt(x, 10) || 0);
while (parts.length < 4) parts.push(0);
const [vA, vB, vC, vD] = parts;

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

const align4 = (n) => (n + 3) & ~3;

function u16(v) {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(v & 0xffff);
  return b;
}
function u32(v) {
  const b = Buffer.alloc(4);
  b.writeUInt32LE(v >>> 0);
  return b;
}
function utf16z(s) {
  const b = Buffer.alloc((s.length + 1) * 2);
  for (let i = 0; i < s.length; i++) b.writeUInt16LE(s.charCodeAt(i), i * 2);
  return b;
}

/**
 * 版本信息块（VS_VERSIONINFO 的通用节点结构）。
 *
 * 每个节点：
 *   WORD wLength       整个节点字节数（含子节点）
 *   WORD wValueLength  Value 的字节数（文本节点是 WCHAR 数）
 *   WORD wType         0=二进制 1=文本
 *   WCHAR szKey[]      以 \0 结尾
 *   填充到 4 字节边界
 *   Value
 *   填充到 4 字节边界
 *   子节点……
 */
function versionNode(key, value, valueLength, type, children) {
  const keyBuf = utf16z(key);
  const parts = [u16(0), u16(valueLength), u16(type), keyBuf];
  let len = parts.reduce((a, b) => a + b.length, 0);
  const pad1 = align4(len) - len;
  parts.push(Buffer.alloc(pad1));

  if (value) parts.push(value);
  len = parts.reduce((a, b) => a + b.length, 0);
  const pad2 = align4(len) - len;
  parts.push(Buffer.alloc(pad2));

  for (const c of children || []) parts.push(c);

  const body = Buffer.concat(parts);
  const head = u16(body.length);
  return Buffer.concat([head, body.subarray(2)]);
}

/** 文本型字符串值：UTF-16 + \0，wValueLength 是 WCHAR 个数（含结尾 0）。 */
function versionString(key, str) {
  const v = utf16z(str);
  return versionNode(key, v, str.length + 1, 1, []);
}

function buildVersionInfo() {
  // VS_FIXEDFILEINFO，52 字节
  const fixed = Buffer.concat([
    u32(0xfeef04bd), // dwSignature
    u32(0x00010000), // dwStrucVersion
    u32((vA << 16) | vB), // dwFileVersionMS
    u32((vC << 16) | vD), // dwFileVersionLS
    u32((vA << 16) | vB), // dwProductVersionMS
    u32((vC << 16) | vD), // dwProductVersionLS
    u32(0x3f), // dwFileFlagsMask
    u32(0), // dwFileFlags
    u32(0x00040004), // dwFileOS = VOS_NT_WINDOWS32
    u32(1), // dwFileType = VFT_APP
    u32(0), // dwFileSubtype
    u32(0), // dwFileDateMS
    u32(0), // dwFileDateLS
  ]);

  const verStr = `${vA}.${vB}.${vC}.${vD}`;
  // 语言 0x0804 = 简体中文，代码页 0x04B0 = 1200(Unicode)
  const strings = [
    ['CompanyName', '黑山大叔'],
    ['FileDescription', 'Minecraft 存档管理器'],
    ['FileVersion', verStr],
    ['InternalName', 'mcworld-app'],
    ['LegalCopyright', 'GPL-3.0'],
    ['OriginalFilename', 'mcworld-app.exe'],
    ['ProductName', 'Minecraft 存档管理器'],
    ['ProductVersion', verStr],
  ].map(([k, v]) => versionString(k, v));

  const stringTable = versionNode('040904B0', null, 0, 1, strings);
  const stringFileInfo = versionNode('StringFileInfo', null, 0, 1, [stringTable]);

  // VarFileInfo → Translation: 语言/代码页
  const translation = versionNode('Translation', u16(0x0409).length === 2
    ? Buffer.concat([u16(0x0409), u16(0x04b0)])
    : Buffer.alloc(0), 4, 0, []);
  const varFileInfo = versionNode('VarFileInfo', null, 0, 1, [translation]);

  return versionNode('VS_VERSION_INFO', fixed, fixed.length, 0, [
    stringFileInfo,
    varFileInfo,
  ]);
}

function buildManifest() {
  const xml = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Heiheid.McSaveManager" version="${vA}.${vB}.${vC}.${vD}" processorArchitecture="amd64"/>
  <description>Minecraft Save Manager</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
      <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">permonitorv2,permonitor</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
</assembly>
`;
  return Buffer.from(xml, 'utf8');
}

// ---------------------------------------------------------------------------
// 从 ICO 拆出各尺寸图像 + 组图标数据
// ---------------------------------------------------------------------------

const ico = fs.readFileSync(icoPath);
const count = ico.readUInt16LE(4);
const images = [];
const groupEntries = [];

for (let i = 0; i < count; i++) {
  const off = 6 + i * 16;
  const w = ico[off];
  const h = ico[off + 1];
  const colors = ico[off + 2];
  const planes = ico.readUInt16LE(off + 4);
  const bpp = ico.readUInt16LE(off + 6);
  const size = ico.readUInt32LE(off + 8);
  const dataOff = ico.readUInt32LE(off + 12);
  images.push(Buffer.from(ico.subarray(dataOff, dataOff + size)));

  const e = Buffer.alloc(14);
  e.writeUInt8(w, 0);
  e.writeUInt8(h, 1);
  e.writeUInt8(colors, 2);
  e.writeUInt8(0, 3);
  e.writeUInt16LE(planes, 4);
  e.writeUInt16LE(bpp, 6);
  e.writeUInt32LE(size, 8);
  e.writeUInt16LE(i + 1, 12);
  groupEntries.push(e);
}

const groupHeader = Buffer.concat([
  u16(0), u16(1), u16(count), // reserved, type=1(icon), count
]);
const group = Buffer.concat([groupHeader, ...groupEntries]);

const versionBlob = buildVersionInfo();
const manifestBlob = buildManifest();

// ---------------------------------------------------------------------------
// 资源树布局
// ---------------------------------------------------------------------------

// 四类资源：(typeId, [ {id, blob} ])
const types = [
  { type: 3, items: images.map((b, i) => ({ id: i + 1, blob: b })) }, // RT_ICON
  { type: 14, items: [{ id: 1, blob: group }] }, // RT_GROUP_ICON
  { type: 16, items: [{ id: 1, blob: versionBlob }] }, // RT_VERSION
  { type: 24, items: [{ id: 1, blob: manifestBlob }] }, // RT_MANIFEST
];

const DIR = 16;
const ENTRY = 8;
const DATAENTRY = 16;

// 目录区大小：root + 每个类型目录 + 每个叶子名目录
let dirsSize = DIR + ENTRY * types.length;
for (const t of types) {
  dirsSize += DIR + ENTRY * t.items.length; // 类型目录
  dirsSize += t.items.length * (DIR + ENTRY); // 叶子名目录
}

const totalItems = types.reduce((a, t) => a + t.items.length, 0);
const dataEntriesSize = totalItems * DATAENTRY;

let cursor = dirsSize + dataEntriesSize;
const blobs = [];
for (const t of types) {
  for (const it of t.items) {
    it.offset = cursor;
    blobs.push(it.blob);
    cursor += align4(it.blob.length);
  }
}
const totalLen = cursor;

const buf = Buffer.alloc(totalLen);

function writeDirHeader(off, count) {
  buf.fill(0, off, off + DIR);
  buf.writeUInt16LE(count, off + 14); // NumberOfIdEntries
}

function writeSubdirEntry(off, id, subOff) {
  buf.writeUInt32LE(id, off); // Name: 纯数字 ID
  buf.writeUInt32LE((subOff | 0x80000000) >>> 0, off + 4); // bit31 → 子目录
}

function writeDataEntry(off, id, dataOff) {
  buf.writeUInt32LE(id, off);
  buf.writeUInt32LE(dataOff, off + 4); // bit31=0 → 数据项
}

// root 目录
// ★ root 目录占 DIR + ENTRY*类型数 字节，第一个类型目录必须从这里开始。
//   早期写成 DIR（16）是错的：那正好落在 root 的第 0 个目录项上，
//   于是解析器在 /3 处读到的"条目数"是目录项的内容（32856）。
let typeDirOff = DIR + ENTRY * types.length;
writeDirHeader(0, types.length);
for (const t of types) {
  writeSubdirEntry(DIR + types.indexOf(t) * ENTRY, t.type, typeDirOff);
  typeDirOff += DIR + ENTRY * t.items.length;
}

// 逐个类型目录
let nameDirOff = DIR + ENTRY * types.length + types.reduce((a, t) => a + DIR + ENTRY * t.items.length, 0);
let dataEntryOff = dirsSize;

let tDirOff = DIR + ENTRY * types.length;
for (const t of types) {
  writeDirHeader(tDirOff, t.items.length);
  for (let i = 0; i < t.items.length; i++) {
    writeSubdirEntry(tDirOff + DIR + i * ENTRY, t.items[i].id, nameDirOff);
    nameDirOff += DIR + ENTRY;
  }
  tDirOff += DIR + ENTRY * t.items.length;
}

// 叶子名目录 + 数据项
nameDirOff = DIR + ENTRY * types.length + types.reduce((a, t) => a + DIR + ENTRY * t.items.length, 0);
for (const t of types) {
  for (const it of t.items) {
    writeDirHeader(nameDirOff, 1);
    writeDataEntry(nameDirOff + DIR, 1, dataEntryOff);
    nameDirOff += DIR + ENTRY;
    dataEntryOff += DATAENTRY;
  }
}

// 数据项。
//
// OffsetToData 必须是**全局 RVA**，但目标文件不知道自己的虚拟地址。
// 这里给它加一条 IMAGE_REL_AMD64_ADDR32NB 重定位，让链接器回填 ——
// 这样链接完就是对的，不需要事后用脚本改 PE。
//
// （早期做法是不加重定位、链接后手工把节内偏移改成 RVA。那个办法能用，
//   但任何没跑修补脚本的产物都是坏 PE —— 比如 `cargo test` 出来的测试
//   程序，Windows 直接报"不是有效的 Win32 应用程序"。）
let deOff = dirsSize;
const relocs = [];
for (const t of types) {
  for (const it of t.items) {
    relocs.push(deOff); // 这 4 字节要被重定位成 RVA
    buf.writeUInt32LE(it.offset, deOff); // 先填节内偏移（重定位的初值）
    buf.writeUInt32LE(it.blob.length, deOff + 4); // Size
    buf.writeUInt32LE(0, deOff + 8); // CodePage
    buf.writeUInt32LE(0, deOff + 12); // Reserved
    deOff += DATAENTRY;
  }
}

// 数据体
for (const t of types) {
  for (const it of t.items) {
    it.blob.copy(buf, it.offset);
  }
}

// ---------------------------------------------------------------------------
// COFF 包装
// ---------------------------------------------------------------------------

const HEADER = 20;
const SECTION = 40;
const RELOC = 10;
const SYM = 18;
const symCount = 2;
const relocTableOff = HEADER + SECTION;
const symTableOff = relocTableOff + relocs.length * RELOC;
const strTableOff = symTableOff + symCount * SYM;
const dataOff = strTableOff + 4;

const out = Buffer.alloc(dataOff + buf.length);
let p = 0;

// IMAGE_FILE_HEADER
out.writeUInt16LE(0x8664, p); p += 2; // Machine = AMD64
out.writeUInt16LE(1, p); p += 2; // NumberOfSections
out.writeUInt32LE(0, p); p += 4; // TimeDateStamp
out.writeUInt32LE(symTableOff, p); p += 4; // PointerToSymbolTable
out.writeUInt32LE(symCount, p); p += 4; // NumberOfSymbols
out.writeUInt16LE(0, p); p += 2; // SizeOfOptionalHeader
out.writeUInt16LE(0, p); p += 2; // Characteristics

// IMAGE_SECTION_HEADER
Buffer.from('.rsrc\0\0\0', 'ascii').copy(out, p); p += 8;
out.writeUInt32LE(buf.length, p); p += 4; // VirtualSize
out.writeUInt32LE(0, p); p += 4; // VirtualAddress
out.writeUInt32LE(buf.length, p); p += 4; // SizeOfRawData
out.writeUInt32LE(dataOff, p); p += 4; // PointerToRawData
out.writeUInt32LE(relocs.length > 0 ? relocTableOff : 0, p); p += 4; // PointerToRelocations
out.writeUInt32LE(0, p); p += 4; // PointerToLinenumbers
out.writeUInt16LE(relocs.length, p); p += 2; // NumberOfRelocations
out.writeUInt16LE(0, p); p += 2; // NumberOfLinenumbers
out.writeUInt32LE(0x40000040, p); p += 4; // INITIALIZED_DATA | MEM_READ

// 重定位表：每条 IMAGE_RELOCATION
//   VirtualAddress(4) SymbolTableIndex(4) Type(2)
// SymbolTableIndex 指向下面的 .rsrc SECTION 符号（索引 0）。
// 早期实现写 0 也能指到它，但 lld 对某些写法会崩，这里显式写 0（即第 0 项）。
for (const r of relocs) {
  out.writeUInt32LE(r, p); p += 4;
  out.writeUInt32LE(0, p); p += 4;
  out.writeUInt16LE(0x0003, p); p += 2; // IMAGE_REL_AMD64_ADDR32NB
}

// 符号表第 0 项：SECTION 符号（名字内联）
Buffer.from('.rsrc\0\0\0', 'ascii').copy(out, p); p += 8;
out.writeUInt32LE(0, p); p += 4; // Value
out.writeInt16LE(1, p); p += 2; // SectionNumber = 1
out.writeUInt16LE(0, p); p += 2; // Type
out.writeUInt8(3, p); p += 1; // StorageClass = IMAGE_SYM_CLASS_STATIC
out.writeUInt8(1, p); p += 1; // NumberOfAuxSymbols = 1

// 第 1 项：aux 记录，**必须正好 18 字节**
out.writeUInt32LE(buf.length, p); p += 4; // Length
out.writeUInt16LE(relocs.length, p); p += 2; // NumberOfRelocations
out.writeUInt16LE(0, p); p += 2; // NumberOfLinenumbers
out.writeUInt32LE(0, p); p += 4; // CheckSum
out.writeUInt16LE(1, p); p += 2; // Number
out.writeUInt8(0, p); p += 1; // Selection
out.writeUInt8(0, p); p += 1; // NumberOfAuxSymbols
out.writeUInt16LE(0, p); p += 2; // padding

// 字符串表长度（空表只有自身 4 字节）
out.writeUInt32LE(4, p); p += 4;

// 节数据
buf.copy(out, p);

fs.writeFileSync(outPath, out);

console.log(`写出 ${outPath}`);
console.log(`  .rsrc ${buf.length} 字节（对齐后 ${totalLen}）`);
console.log(`  资源: RT_ICON x${images.length}, RT_GROUP_ICON, RT_VERSION(${versionBlob.length}B), RT_MANIFEST(${manifestBlob.length}B)`);
console.log(`  重定位 ${relocs.length} 条（ADDR32NB，由链接器回填 RVA）`);
console.log(`  版本 ${vA}.${vB}.${vC}.${vD}`);
