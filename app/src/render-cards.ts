// Renders every member's share card from the live chain and writes static share pages:
//   web/cards/<name>.png      the holo card (750x1050)
//   web/cards/<name>-og.jpg   landscape preview (1200x630) for X / Slack / Discord unfurls
//   web/cards/<name>.html     og:image + twitter:card tags, then forwards to the board (#m=<name>)
//   web/art/og.jpg            the site-wide preview (the title hero)
// Link previews are fetched by crawlers without running JavaScript, so these have to be files.
//   npm run render-cards
// Runs in CI (.github/workflows/cards.yml) on every push and every 6 hours.
import fs from "node:fs";
import path from "node:path";
import { chromium } from "playwright";
import { env, view } from "./near.js";

const SITE = process.env.SITE_URL ?? "https://guildhall.designerdev.workers.dev";
const WEB = path.resolve(process.cwd(), "../web");
const OUT = path.join(WEB, "cards");
const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;");
const LEVEL = ["", "Bronze", "Silver", "Gold"];

async function main() {
  const hub = env("HUB_CONTRACT");
  const agents: any[] = await view(hub, "list_agents", {});
  fs.mkdirSync(OUT, { recursive: true });
  const browser = await chromium.launch({ args: ["--allow-file-access-from-files"] });
  const page = await browser.newPage({ viewport: { width: 1362, height: 1000 } });
  page.on("pageerror", (e: Error) => console.error("page error:", e.message));

  // Site-wide preview: the title hero at 1200x630.
  // Night theme, header hidden, the hero filling the frame edge to edge.
  await page.setViewportSize({ width: 1200, height: 630 });
  await page.addInitScript(() => { try { localStorage.setItem("guildhall.theme", "night"); } catch {} });
  await page.goto(`file://${WEB}/index.html`);
  await page.addStyleTag({ content: "header{display:none!important} body{padding:0!important;margin:0!important} .hero{min-height:630px!important;border:0!important;border-radius:0!important} .wrap,main{padding:0!important;margin:0!important;max-width:none!important}" });
  await page.waitForTimeout(3500);
  await page.mouse.move(820, 300);
  await page.waitForTimeout(300);
  const hero = await page.$("#hero");
  const box = await hero!.boundingBox();
  await page.screenshot({ path: path.join(WEB, "art", "og.jpg"), type: "jpeg", quality: 88, clip: { x: box!.x, y: box!.y, width: Math.min(1200, box!.width), height: 630 } });
  await page.setViewportSize({ width: 1362, height: 1000 });

  for (const a of agents) {
    const slug = a.account.split(".")[0];
    // The page reads the chain itself and shared CI runners get rate-limited by the public
    // RPC, so a load can come up empty. Give it time, then reload and try again.
    for (let attempt = 1; ; attempt++) {
      await page.goto(`file://${WEB}/index.html#m=${slug}`);
      await page.reload();
      try { await page.click("#sharebtn", { timeout: 45_000 }); break; }
      catch (e) {
        if (attempt >= 4) throw e;
        console.warn(`${slug}: board did not load (attempt ${attempt}/4), waiting ${20 * attempt}s`);
        await page.waitForTimeout(20_000 * attempt);
      }
    }
    await page.waitForFunction("window.__cardReady === true", null, { timeout: 30_000 });
    await page.waitForTimeout(400);
    const card: string = await page.evaluate("document.getElementById('cardcanvas').toDataURL('image/png')");
    const og: string = await page.evaluate("window.__guildhall.renderOg().then((u) => { const c = document.createElement('canvas'); const i = new Image(); return new Promise((res) => { i.onload = () => { c.width = i.width; c.height = i.height; c.getContext('2d').drawImage(i, 0, 0); res(c.toDataURL('image/jpeg', 0.88)); }; i.src = u; }); })");
    fs.writeFileSync(path.join(OUT, `${slug}.png`), Buffer.from(card.split(",")[1], "base64"));
    fs.writeFileSync(path.join(OUT, `${slug}-og.jpg`), Buffer.from(og.split(",")[1], "base64"));

    // One at a time: the page above has just made its own burst of reads against the same RPC.
    const certs = await view(hub, "get_certs", { agent: a.account });
    const rank = await view(hub, "get_rank", { agent: a.account });
    const revs = await view(hub, "get_revocations", { agent: a.account }).catch(() => []);
    await new Promise((r) => setTimeout(r, 6000));
    const now = Date.now() * 1e6;
    const cur = (certs as any[]).filter((c) => Number(c.expires_at) > now).sort((x, y) => Number(y.issued_at) - Number(x.issued_at))[0];
    const seal = cur ? `${LEVEL[cur.level]} seal` : (revs as any[]).length ? `${LEVEL[(revs as any[]).at(-1).level]} seal burned` : "no seal";
    const name = a.display_name || slug;
    const title = `${name} — ${rank.name}, ${seal} · Guildhall`;
    const desc = `${a.jobs_approved}/${a.jobs_completed} contracts approved on NEAR. Seals are won in trials and burned on failure; ranks are computed by the contract. Verify it at ${hub}.`;
    const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>${esc(title)}</title>
<meta name="description" content="${esc(desc)}">
<meta property="og:type" content="profile"><meta property="og:site_name" content="Guildhall">
<meta property="og:title" content="${esc(title)}"><meta property="og:description" content="${esc(desc)}">
<meta property="og:url" content="${SITE}/cards/${slug}">
<meta property="og:image" content="${SITE}/cards/${slug}-og.jpg"><meta property="og:image:width" content="1200"><meta property="og:image:height" content="630">
<meta name="twitter:card" content="summary_large_image"><meta name="twitter:title" content="${esc(title)}"><meta name="twitter:description" content="${esc(desc)}"><meta name="twitter:image" content="${SITE}/cards/${slug}-og.jpg">
<meta http-equiv="refresh" content="0; url=/#m=${slug}">
<style>body{margin:0;background:#0b0c0e;color:#d9d4c7;font:16px Georgia,serif;display:grid;place-items:center;min-height:100vh}a{color:#cdb878}</style>
</head><body><p>Opening ${esc(name)} in the Guildhall… <a href="/#m=${slug}">continue</a></p>
<script>location.replace("/#m=${slug}")</script></body></html>
`;
    fs.writeFileSync(path.join(OUT, `${slug}.html`), html);
    console.log(`${slug}: ${rank.name}, ${seal}`);
  }
  await browser.close();
  console.log(`wrote ${agents.length} card pages to ${OUT}`);
}

main().catch((e) => { console.error(e); process.exit(1); });
