#!/usr/bin/env node
/**
 * Tiny static server that sets the Cross-Origin headers required for
 * SharedArrayBuffer / wasm threads. Same headers as itch.io SharedArrayBuffer option.
 */
import http from "node:http";
import fs from "node:fs/promises";
import path from "node:path";

const PORT = Number(process.argv[2]) || 8080;
const ROOT = process.cwd();

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "application/javascript",
  ".wasm": "application/wasm",
  ".opus": "audio/ogg",
  ".css": "text/css",
  ".json": "application/json",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".ico": "image/x-icon",
  ".webp": "image/webp",
};

const ISOLATION_HEADERS = {
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Embedder-Policy": "require-corp",
  "Cross-Origin-Resource-Policy": "same-origin",
};

function underRoot(resolved) {
  const rel = path.relative(ROOT, resolved);
  return !rel.startsWith("..") && !path.isAbsolute(rel);
}

const server = http.createServer(async (req, res) => {
  let pathname;
  try {
    pathname = new URL(req.url ?? "/", "http://127.0.0.1").pathname;
  } catch {
    res.writeHead(400, { "Content-Type": "text/plain", ...ISOLATION_HEADERS });
    res.end("Bad request");
    return;
  }

  let rel =
    pathname === "/" || pathname === ""
      ? "index.html"
      : path.normalize(decodeURIComponent(pathname.replace(/^\/+/, "")));
  if (rel.startsWith("..")) {
    res.writeHead(403, { "Content-Type": "text/plain", ...ISOLATION_HEADERS });
    res.end("Forbidden");
    return;
  }

  let filePath = path.resolve(ROOT, rel);
  if (!underRoot(filePath)) {
    res.writeHead(403, { "Content-Type": "text/plain", ...ISOLATION_HEADERS });
    res.end("Forbidden");
    return;
  }

  try {
    let st = await fs.stat(filePath);
    if (st.isDirectory()) {
      filePath = path.join(filePath, "index.html");
      st = await fs.stat(filePath);
    }
    const body = await fs.readFile(filePath);
    const ext = path.extname(filePath).toLowerCase();
    const contentType = MIME[ext] ?? "application/octet-stream";
    res.writeHead(200, { "Content-Type": contentType, ...ISOLATION_HEADERS });
    res.end(body);
  } catch {
    res.writeHead(404, { "Content-Type": "text/plain", ...ISOLATION_HEADERS });
    res.end("Not found");
  }
});

server.listen(PORT, () => {
  console.log(`serving on http://localhost:${PORT}  (COOP+COEP set)`);
});
