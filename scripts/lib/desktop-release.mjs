const stable = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

function parts(version) {
  if (!stable.test(version)) throw new Error('Automatic desktop releases require VERSION in X.Y.Z format');
  return version.split('.').map(BigInt);
}

export function compareVersions(left, right) {
  const a = parts(left), b = parts(right);
  for (let i = 0; i < 3; i++) {
    if (a[i] !== b[i]) return a[i] > b[i] ? 1 : -1;
  }
  return 0;
}

export function nextReleaseVersion(version, tags) {
  parts(version);
  const versions = tags.filter(tag => tag.startsWith('v') && stable.test(tag.slice(1))).map(tag => tag.slice(1));
  const latest = versions.reduce((a, b) => !a || compareVersions(b, a) > 0 ? b : a, null);
  if (!latest || compareVersions(version, latest) > 0) return version;
  const [major, minor, patch] = parts(latest);
  return `${major}.${minor}.${patch + 1n}`;
}

export function downloadSection(version, date) {
  parts(version);
  const base = `https://github.com/yertYang/Lorevia/releases/download/v${version}/`;
  return `<!-- desktop-downloads:start -->
当前最新正式版为 **[v${version}](https://github.com/yertYang/Lorevia/releases/tag/v${version})**，发布于 ${date}。请选择适合自己设备的安装包：

| 操作系统 | 适用设备 | 下载 v${version} |
| --- | --- | --- |
| macOS | Apple Silicon（M 系列芯片） | [下载 DMG](${base}Lorevia-MacOS-Apple-Silicon-v${version}.dmg) |
| macOS | Intel 芯片 | [下载 DMG](${base}Lorevia-MacOS-Intel-v${version}.dmg) |
| Windows | x64 安装版 | [下载 Setup.exe](${base}Lorevia-Windows-v${version}-Setup.exe) |
| Windows | x64 便携版 | [下载 Portable.zip](${base}Lorevia-Windows-v${version}-Portable.zip) |
<!-- desktop-downloads:end -->`;
}

export function updateReadme(readme, version, date) {
  const section = /<!-- desktop-downloads:start -->[\s\S]*?<!-- desktop-downloads:end -->/;
  if (!section.test(readme)) throw new Error('README.md is missing its desktop download section markers');
  return readme.replace(section, () => downloadSection(version, date));
}
