// The packaged app runs under *two* content security policies: the meta tag in
// each page, and the header derived from `app.security.csp` in tauri.conf.json.
// The webview intersects them, so a directive that is only in the meta tag is
// not merely redundant — it is blocked, and only inside an installed build where
// no browser preview will show it.
//
// The header policy is application-wide, the meta tags are per page, so the rule
// is one-directional: the header must permit everything every page declares.

const fs = require("node:fs")
const path = require("node:path")

const SOURCE_LIST = new Set([
  "default-src", "script-src", "style-src", "img-src", "font-src", "connect-src",
  "media-src", "object-src", "frame-src", "worker-src", "child-src", "manifest-src",
  "prefetch-src", "form-action", "base-uri", "frame-ancestors", "sandbox",
])

function parsePolicy(text) {
  const directives = new Map()
  for (const part of String(text).split(";")) {
    const tokens = part.trim().split(/\s+/).filter(Boolean)
    if (tokens.length === 0) continue
    const name = tokens.shift().toLowerCase()
    if (directives.has(name)) throw new Error(`directive ${name} appears twice`)
    directives.set(name, tokens)
  }
  return directives
}

function pagePolicies(root) {
  const dir = path.join(root, "ui")
  const pages = fs.readdirSync(dir).filter(name => name.endsWith(".html")).sort()
  return pages.map(page => {
    const html = fs.readFileSync(path.join(dir, page), "utf8")
    const match = html.match(/<meta\s+http-equiv="Content-Security-Policy"\s+content="([^"]*)"/i)
    if (!match) throw new Error(`ui/${page} has no meta Content-Security-Policy`)
    return { page, directives: parsePolicy(match[1]) }
  })
}

function headerPolicy(root) {
  const conf = JSON.parse(fs.readFileSync(path.join(root, "src-tauri/tauri.conf.json"), "utf8"))
  const csp = conf?.app?.security?.csp
  if (typeof csp !== "string" || csp.trim() === "") {
    throw new Error("tauri.conf.json has no app.security.csp")
  }
  return parsePolicy(csp)
}

// Returns the violations instead of throwing so the caller can decide.
function compareCsp(root) {
  const header = headerPolicy(root)
  const problems = []
  for (const { page, directives } of pagePolicies(root)) {
    for (const [name, sources] of directives) {
      if (!header.has(name)) {
        problems.push(`ui/${page}: ${name} is missing from tauri.conf.json, so the packaged app falls back to its default-src`)
        continue
      }
      if (!SOURCE_LIST.has(name)) continue
      const allowed = new Set(header.get(name))
      for (const source of sources) {
        if (!allowed.has(source)) {
          problems.push(`ui/${page}: ${name} allows ${source}, which tauri.conf.json does not`)
        }
      }
    }
  }
  return problems
}

function checkCsp(root = path.join(__dirname, "..")) {
  const problems = compareCsp(root)
  if (problems.length > 0) {
    throw new Error(`Content Security Policy mismatch:\n- ${problems.join("\n- ")}`)
  }
  return true
}

module.exports = { parsePolicy, compareCsp, checkCsp }

if (require.main === module) {
  try {
    checkCsp()
    console.log("CSP policies agree.")
  } catch (error) {
    console.error(error.message)
    process.exit(1)
  }
}
