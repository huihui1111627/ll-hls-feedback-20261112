// 模拟 N 个 LL-HLS 观众：blocking reload 跟播放列表 + 拉 part + 拉完整 segment。
// 用法：VIEWERS=500 DURATION=90 node scripts/loadtest.mjs
const BASE = process.env.BASE || 'http://127.0.0.1:8080';
const REND = process.env.REND || '480p';
const VIEWERS = Number(process.env.VIEWERS || 500);
const DURATION = Number(process.env.DURATION || 60);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function percentile(arr, p) {
  if (!arr.length) return 0;
  const sorted = [...arr].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))];
}

const stats = {
  playlist: [],
  media: [],
  errors: 0,
  requests: 0,
  bytes: 0,
};

async function get(url) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 5000);
  try {
    const res = await fetch(url, { signal: controller.signal });
    const buf = Buffer.from(await res.arrayBuffer());
    stats.bytes += buf.length;
    stats.requests++;
    if (!res.ok) stats.errors++;
    return { ok: res.ok, text: buf.toString('utf8'), len: buf.length };
  } catch {
    stats.errors++;
    return { ok: false, text: '', len: 0 };
  } finally {
    clearTimeout(timer);
  }
}

async function viewer(id) {
  let msn = null;
  let nextPart = 0;
  let lastSeg;
  let started = false;

  // 等直播开始
  while (!started) {
    const r = await get(`${BASE}/live/${REND}/index.m3u8`);
    const m = [...r.text.matchAll(/part\/(\d+)\/(\d+)\.m4s/g)];
    if (m.length) {
      msn = Number(m[m.length - 1][1]);
      lastSeg = Number([...r.text.matchAll(/seg\/(\d+)\.m4s/g)].pop()?.[1] ?? -1);
      started = true;
    } else {
      await sleep(500 + Math.random() * 500);
    }
  }

  const deadline = Date.now() + DURATION * 1000;
  while (Date.now() < deadline) {
    const t0 = Date.now();
    const r = await get(`${BASE}/live/${REND}/index.m3u8?_HLS_msn=${msn}&_HLS_part=${nextPart}`);
    stats.playlist.push(Date.now() - t0);

    const m = [...r.text.matchAll(/part\/(\d+)\/(\d+)\.m4s/g)];
    if (!m.length) { await sleep(300); continue; }
    const latest = m[m.length - 1];
    const latestMsn = Number(latest[1]);
    const latestIdx = Number(latest[2]);

    if (latestMsn > msn) {
      // 新的 segment 出现了，补拉一个完整 segment
      const seg = [...r.text.matchAll(/seg\/(\d+)\.m4s/g)].pop();
      if (seg && Number(seg[1]) > lastSeg) {
        const tt = Date.now();
        await get(`${BASE}/live/${REND}/seg/${seg[1]}.m4s`);
        stats.media.push(Date.now() - tt);
        lastSeg = Number(seg[1]);
      }
      msn = latestMsn;
      nextPart = 0;
    }

    // 拉还没拉过的 part（最多补 4 个，避免追不上时堆积）
    if (latestMsn === msn) {
      let n = 0;
      while (nextPart <= latestIdx && n < 4) {
        const tt = Date.now();
        await get(`${BASE}/live/${REND}/part/${msn}/${nextPart}.m4s`);
        stats.media.push(Date.now() - tt);
        nextPart++;
        n++;
      }
    }
  }
}

const t0 = Date.now();
await Promise.all(Array.from({ length: VIEWERS }, (_, i) => viewer(i)));
const dur = (Date.now() - t0) / 1000;

console.log(JSON.stringify({
  viewers: VIEWERS,
  duration_s: +dur.toFixed(1),
  requests: stats.requests,
  rps: +(stats.requests / dur).toFixed(0),
  bytes_mb: +(stats.bytes / 1e6).toFixed(1),
  mbps: +((stats.bytes * 8) / dur / 1e6).toFixed(1),
  errors: stats.errors,
  playlist_ms: { p50: percentile(stats.playlist, 50), p95: percentile(stats.playlist, 95), max: Math.max(...stats.playlist) },
  media_ms: { p50: percentile(stats.media, 50), p95: percentile(stats.media, 95), max: Math.max(...stats.media) },
}, null, 2));
