const assert = require("node:assert/strict")
const fs = require("node:fs")
const os = require("node:os")
const path = require("node:path")
const { test } = require("node:test")
const { checkCsp, compareCsp, parsePolicy } = require("./check-csp")

const HEADER = "default-src 'none'; img-src 'self'; media-src 'self'; style-src 'self' 'unsafe-inline'"

function fixture(t, { header = HEADER, pages = { "index.html": HEADER } } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "pi-csp-"))
  t.after(() => fs.rmSync(root, { recursive: true, force: true }))
  fs.mkdirSync(path.join(root, "ui"))
  fs.mkdirSync(path.join(root, "src-tauri"))
  for (const [name, policy] of Object.entries(pages)) {
    fs.writeFileSync(
      path.join(root, "ui", name),
      `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="${policy}"></head><body></body></html>`,
    )
  }
  fs.writeFileSync(
    path.join(root, "src-tauri/tauri.conf.json"),
    JSON.stringify({ app: { security: { csp: header } } }),
  )
  return root
}

test("parsing keeps directive order irrelevant and rejects duplicates", () => {
  assert.deepEqual([...parsePolicy("img-src 'self'; media-src 'self'").keys()], ["img-src", "media-src"])
  assert.throws(() => parsePolicy("img-src 'self'; img-src data:"), /appears twice/)
})

test("every page's policy has to be permitted by the shipped header", t => {
  const root = fixture(t, {
    pages: {
      "index.html": HEADER,
      "tray.html": HEADER.replace("img-src 'self'", "img-src 'self' data:"),
    },
  })
  assert.deepEqual(compareCsp(root), [
    "ui/tray.html: img-src allows data:, which tauri.conf.json does not",
  ])
  assert.throws(() => checkCsp(root), /CSP mismatch|Content Security Policy mismatch/)
})

test("a directive only in the page is blocked once the app is installed", t => {
  // The page allows it, the header falls back to default-src 'none' and the
  // installed webview refuses the request while every browser preview passes.
  const root = fixture(t, {
    header: HEADER.replace("; media-src 'self'", ""),
  })
  assert.deepEqual(compareCsp(root), [
    "ui/index.html: media-src is missing from tauri.conf.json, so the packaged app falls back to its default-src",
  ])
})

test("a missing meta policy or an absent csp fails closed", t => {
  const root = fixture(t)
  fs.writeFileSync(path.join(root, "ui/index.html"), "<!doctype html><html><head></head><body></body></html>")
  assert.throws(() => compareCsp(root), /no meta Content-Security-Policy/)
  const g = fixture(t)
  fs.writeFileSync(path.join(g, "src-tauri/tauri.conf.json"), JSON.stringify({ app: { security: {} } }))
  assert.throws(() => compareCsp(g), /no app.security.csp/)
})

test("the repository's real pages and configuration agree", () => {
  assert.equal(checkCsp(), true)
})
