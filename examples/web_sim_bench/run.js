// Drive examples/web_sim_bench in headless Chromium (Playwright): check
// that the JIT ends in the same state as the interpreter, then time both,
// interleaved, and print ticks per second.
//
//   node examples/web_sim_bench/run.js <wasm-bindgen out dir> [design:ticks ...]
//
// Environment: REPS (rounds, default 5), SECONDS (per measurement, 2),
// WARM (warm-up seconds, 1), CHUNK (ticks per run call, 4096), DEBUG=1
// (compile with Options::debug, as the editor does).
const http = require('http');
const fs = require('fs');
const path = require('path');
const { chromium } = require('playwright');

const dir = path.resolve(process.argv[2]);
const cases = (process.argv.slice(3).length ? process.argv.slice(3) : ['nes:200000', 'cpu16:200000'])
  .map((c) => { const [name, ticks] = c.split(':'); return { name, ticks: +ticks }; });
const reps = +(process.env.REPS || 5);
const seconds = +(process.env.SECONDS || 2);
const warm = +(process.env.WARM || 1);
const chunk = +(process.env.CHUNK || 4096);
const debug = process.env.DEBUG === '1';

const types = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm' };
const server = http.createServer((req, res) => {
  const name = req.url === '/' ? 'index.html' : req.url.slice(1).split('?')[0];
  const file = name === 'index.html' ? path.join(__dirname, name) : path.join(dir, path.basename(name));
  fs.readFile(file, (err, data) => {
    if (err) { res.writeHead(404); res.end(); return; }
    res.writeHead(200, { 'Content-Type': types[path.extname(file)] || 'application/octet-stream' });
    res.end(data);
  });
});

const median = (xs) => { const s = [...xs].sort((a, b) => a - b); return s[Math.floor(s.length / 2)]; };

server.listen(0, async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage();
  page.on('console', (msg) => console.log(`  [page] ${msg.text()}`));
  page.on('pageerror', (e) => console.log(`  [page error] ${e}`));
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  await page.waitForFunction(() => window.benchReady, null, { timeout: 120000 });
  let ok = true;
  for (const c of cases) {
    const a = await page.evaluate((x) => check(x.name, false, x.debug, x.ticks, x.chunk), { ...c, debug, chunk });
    const b = await page.evaluate((x) => check(x.name, true, x.debug, x.ticks, x.chunk), { ...c, debug, chunk });
    const same = a.digest === b.digest && b.jit;
    ok &&= same;
    console.log(`${c.name} (${a.info}): after ${c.ticks} ticks interpreter ${a.digest}, JIT ${b.digest} ` +
      `(wasm code ${b.jit ? 'active' : 'NOT active'}, ready in ${b.readyMs.toFixed(0)} ms): ${same ? 'same' : 'DIFFERENT'}`);
  }
  const rates = {};
  for (let r = 0; r < reps; r++) {
    for (const c of cases) {
      for (const jit of r % 2 ? [true, false] : [false, true]) {
        const x = await page.evaluate((y) => bench(y.name, y.jit, y.debug, y.seconds, y.chunk, y.warm),
          { name: c.name, jit, debug, seconds, chunk, warm });
        const k = `${c.name} ${jit ? 'jit' : 'interp'}`;
        (rates[k] ||= []).push(x.rate);
        console.log(`  round ${r + 1}: ${k}: ${(x.rate / 1e3).toFixed(1)} k ticks/s`);
      }
    }
  }
  if (!reps) { await browser.close(); server.close(); process.exit(ok ? 0 : 1); }
  console.log(`\nmedian of ${reps} (${seconds} s each, run(${chunk}), ${debug ? 'Options::debug' : 'Options::default'}):`);
  for (const c of cases) {
    const i = median(rates[`${c.name} interp`]);
    const j = median(rates[`${c.name} jit`]);
    console.log(`${c.name}: interpreter ${(i / 1e3).toFixed(1)} k ticks/s, wasm JIT ${(j / 1e3).toFixed(1)} k ticks/s, ${(j / i).toFixed(2)}x`);
  }
  await browser.close();
  server.close();
  process.exit(ok ? 0 : 1);
});
