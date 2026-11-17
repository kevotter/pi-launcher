// Startup screen behaviour: the scrolling "Ciallo" boards and the bouncing
// title. Nothing responds to a click — the screen is an animation, not a toy.
//
// Kept as a module (not inline) because the page's CSP allows `script-src
// 'self'` only. No audio: the window appears at login, often before the user is
// at the desk.

const TEXT = 'Ciallo～(∠・ω< )⌒★';

/* ---------------- 中央跳动标题 ---------------- */
const jumper = document.getElementById('jumper');
const chars = Array.from(TEXT);
const count = chars.length;
chars.forEach((ch, index) => {
  const span = document.createElement('span');
  span.textContent = ch;
  // 依次起跳，形成波浪
  span.style.animationDelay = (0.3 * index / count).toFixed(3) + 's';
  jumper.appendChild(span);
});

/* ---------------- 滚动弹幕：17 条轨道 ---------------- */
// dur 决定滚动速度，size 决定字号——沿用原站的错落感，颜色调深以便在浅底上看得清
const TRACKS = [
  { dur: 12, color: '#e04a44', size: 42 },
  { dur: 15, color: '#0e8f9c', size: 46 },
  { dur: 11, color: '#ef6038', size: 30 },
  { dur: 7, color: '#3a2f38', size: 22 },
  { dur: 10, color: '#5f8f00', size: 34 },
  { dur: 12, color: '#b98200', size: 22 },
  { dur: 15, color: '#e0700a', size: 56 },
  { dur: 11, color: '#d94f8f', size: 64 },
  { dur: 12, color: '#8a8a8a', size: 50 },
  { dur: 18, color: '#0a7f96', size: 34 },
  { dur: 11, color: '#5f8f00', size: 26 },
  { dur: 14, color: '#7b7b7b', size: 22 },
  { dur: 6, color: '#8a4fbd', size: 48 },
  { dur: 15, color: '#2a62c8', size: 32 },
  { dur: 14, color: '#2b8f4c', size: 54 },
  { dur: 12, color: '#0e8f9c', size: 72 },
  { dur: 11, color: '#3a2f38', size: 28 },
];

const stage = document.getElementById('stage');
const H_BASE = 1.35; // 轨道高度 = 字号 * 1.35

function buildTracks() {
  const vh = window.innerHeight;
  let total = 0;
  TRACKS.forEach((track) => { total += track.size * H_BASE; });

  // 小屏按比例整体缩小，保证 17 行塞得进一屏
  const scale = Math.min(1, (vh * 0.995) / total);
  let y = Math.max(0, (vh - total * scale) / 2);

  stage.textContent = '';
  TRACKS.forEach((track, ti) => {
    const size = track.size * scale;
    const row = document.createElement('div');
    row.className = 'track';
    row.style.top = y.toFixed(1) + 'px';
    row.style.height = (size * H_BASE).toFixed(1) + 'px';

    // 起始相位错开，开场就不是所有行整齐地从右边一起出发
    const phase = ((ti * 0.618) % 1) * track.dur;

    // 同一条轨道放两份，错开半个周期，滚动才不会断流
    for (let k = 0; k < 2; k++) {
      const span = document.createElement('span');
      span.className = 'ciallo';
      span.textContent = TEXT;
      span.style.fontSize = size.toFixed(1) + 'px';
      span.style.color = track.color;
      span.style.setProperty('--dur', track.dur + 's');
      span.style.animationDelay = '-' + (phase + k * track.dur / 2).toFixed(2) + 's';
      row.appendChild(span);
    }

    stage.appendChild(row);
    y += size * H_BASE;
  });
}

buildTracks();

let resizeTimer = null;
window.addEventListener('resize', () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(buildTracks, 180);
});

/* ---------------- 窗口行为 ---------------- */
// 这里不是网页，没人指望右键有菜单，中键也不该触发 Chromium 的自动滚动。
// 失败面板除外，那是阅读面，要能右键复制日志路径。
function isReading(target) {
  return target instanceof Element && target.closest('.failure') !== null;
}

document.addEventListener('contextmenu', (event) => {
  if (isReading(event.target)) return;
  event.preventDefault();
});

document.addEventListener('mousedown', (event) => {
  if (event.button === 1 && !isReading(event.target)) event.preventDefault();
});
