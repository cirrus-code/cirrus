// Investigative probe: load a Salesforce help page in headless Chromium,
// log every HTML/JSON/XML response the page fetches, and emit a JSON
// summary on stdout.
//
// Goal: identify the content-API endpoint that the Lightning SPA hits to
// load article bodies, so sf-doc.nu can hit that endpoint directly with
// curl instead of driving a full browser through single-file-cli.
//
// Usage (via the nu wrapper which manages puppeteer-core):
//   nu scripts/sf-doc-probe.nu '<URL>'

const puppeteer = require("puppeteer-core");
const { execSync } = require("node:child_process");

(async () => {
  const url = process.argv[2];
  if (!url) {
    console.error("usage: sf-doc-probe.js <url>");
    process.exit(2);
  }

  const chromium =
    process.env.CHROMIUM_PATH ||
    execSync("which chromium").toString().trim();

  const browser = await puppeteer.launch({
    executablePath: chromium,
    headless: "new",
    args: ["--no-sandbox", "--disable-gpu"],
  });
  const page = await browser.newPage();

  await page.setViewport({ width: 1400, height: 4000 });

  const responses = [];
  page.on("response", async (r) => {
    let preview = "";
    try {
      const u = r.url();
      const s = r.status();
      const h = r.headers();
      const ct = h["content-type"] || "";
      if (s === 200 && /text\/html|application\/json|application\/xml/i.test(ct)) {
        try {
          const buf = await r.buffer();
          preview = buf.toString("utf8").slice(0, 240).replace(/\s+/g, " ");
        } catch {
          // Some responses can't be re-read after preflight; skip preview.
        }
      }
      responses.push({
        url: u,
        status: s,
        contentType: ct,
        contentLength: parseInt(h["content-length"] || "0", 10) || 0,
        resourceType: r.request().resourceType(),
        preview,
      });
    } catch {
      // Best-effort logging; never let a bad response abort the probe.
    }
  });

  try {
    await page.goto(url, { waitUntil: "networkidle0", timeout: 60_000 });
  } catch (e) {
    console.error(`navigation error: ${e.message}`);
  }

  await new Promise((res) => setTimeout(res, 8000));

  const interesting = responses
    .filter((r) => r.status === 200)
    .filter((r) =>
      /text\/html|application\/json|application\/xml/i.test(r.contentType),
    )
    .filter((r) => r.url !== url)
    .filter(
      (r) =>
        !/google|gstatic|googletagmanager|adobe|cookielaw|onetrust|cookiebot|hotjar|salesforce\.com\/css/i.test(
          r.url,
        ),
    )
    .sort((a, b) => {
      if (a.resourceType !== b.resourceType) {
        return a.resourceType.localeCompare(b.resourceType);
      }
      return (b.contentLength || 0) - (a.contentLength || 0);
    });

  console.log(JSON.stringify(interesting, null, 2));
  await browser.close();
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
