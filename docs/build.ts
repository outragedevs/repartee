/**
 * repartee Documentation — Static Site Builder
 *
 * Reads markdown content, converts to HTML via `marked`, injects into a
 * page template, and writes the final site to docs/.
 *
 * Usage:  bun run docs/build.ts
 */

import { marked } from "marked";
import { join, basename } from "path";

const ROOT = import.meta.dir;
const CONTENT_DIR = join(ROOT, "src/content");
const TEMPLATE_PATH = join(ROOT, "src/templates/page.html");
const CSS_SRC = join(ROOT, "src/css/style.css");
const JS_SRC_DIR = join(ROOT, "src/js");
const COMMANDS_DIR = join(ROOT, "commands");
const SITE_DIR = ROOT;

interface SiteEntry {
  slug: string;
  title: string;
  section?: string;
  source?: string;
}

const siteMap: SiteEntry[] = [
  { slug: "index", title: "Home" },
  { slug: "installation", title: "Installation", section: "Getting Started" },
  { slug: "first-connection", title: "First Connection", section: "Getting Started" },
  { slug: "configuration", title: "Configuration", section: "Getting Started" },
  { slug: "commands", title: "Commands", section: "Reference" },
  { slug: "scripting-getting-started", title: "Getting Started", section: "Scripting" },
  { slug: "scripting-api", title: "API Reference", section: "Scripting" },
  { slug: "scripting-examples", title: "Examples", section: "Scripting" },
  { slug: "theming", title: "Theming", section: "Customization" },
  { slug: "theming-format-strings", title: "Format Strings", section: "Customization" },
  { slug: "logging", title: "Logging & Search", section: "Customization" },
  { slug: "architecture", title: "Architecture", section: "Project" },
  { slug: "faq", title: "FAQ", section: "Project" },
];

async function readText(path: string): Promise<string> {
  const file = Bun.file(path);
  if (!(await file.exists())) return "";
  return file.text();
}

function parseFrontMatter(raw: string): { meta: Record<string, string>; body: string } {
  const lines = raw.split("\n");
  if (lines[0]?.trim() !== "---") return { meta: {}, body: raw };
  let end = -1;
  for (let i = 1; i < lines.length; i++) {
    if (lines[i]?.trim() === "---") { end = i; break; }
  }
  if (end === -1) return { meta: {}, body: raw };
  const meta: Record<string, string> = {};
  for (let i = 1; i < end; i++) {
    const m = lines[i]!.match(/^(\w[\w-]*):\s*(.+)$/);
    if (m) meta[m[1]!] = m[2]!.trim();
  }
  return { meta, body: lines.slice(end + 1).join("\n") };
}

function buildNav(activeSlug: string): string {
  let html = "";
  let currentSection: string | undefined;
  for (const entry of siteMap) {
    if (!entry.section) {
      if (currentSection !== undefined) { html += `    </ul>\n  </div>\n`; currentSection = undefined; }
      const cls = entry.slug === activeSlug ? ' class="active"' : "";
      html += `  <ul>\n    <li><a href="${entry.slug}.html"${cls}>${entry.title}</a></li>\n  </ul>\n`;
      continue;
    }
    if (entry.section !== currentSection) {
      if (currentSection !== undefined) html += `    </ul>\n  </div>\n`;
      currentSection = entry.section;
      html += `  <div class="nav-section">\n    <span class="nav-section-title">${currentSection}</span>\n    <ul>\n`;
    }
    const cls = entry.slug === activeSlug ? ' class="active"' : "";
    html += `      <li><a href="${entry.slug}.html"${cls}>${entry.title}</a></li>\n`;
  }
  if (currentSection !== undefined) html += `    </ul>\n  </div>\n`;
  return html;
}

function buildPrevNext(index: number): { prev: string; next: string } {
  let prev = "", next = "";
  if (index > 0) {
    const p = siteMap[index - 1]!;
    prev = `<a href="${p.slug}.html" class="page-nav-link prev">\n  <span class="page-nav-label">&larr; Previous</span>\n  <span class="page-nav-title">${p.title}</span>\n</a>`;
  }
  if (index < siteMap.length - 1) {
    const n = siteMap[index + 1]!;
    next = `<a href="${n.slug}.html" class="page-nav-link next">\n  <span class="page-nav-label">Next &rarr;</span>\n  <span class="page-nav-title">${n.title}</span>\n</a>`;
  }
  return { prev, next };
}

interface CommandEntry { name: string; category: string; description: string; html: string; }

async function buildCommandsPage(): Promise<string> {
  const glob = new Bun.Glob("*.md");
  const files: string[] = [];
  for await (const path of glob.scan(COMMANDS_DIR)) files.push(path);
  files.sort();
  const commands: CommandEntry[] = [];
  for (const file of files) {
    const raw = await Bun.file(join(COMMANDS_DIR, file)).text();
    const { meta, body } = parseFrontMatter(raw);
    commands.push({ name: basename(file, ".md"), category: meta.category || "Other", description: meta.description || "", html: await marked.parse(body) });
  }
  const categories = new Map<string, CommandEntry[]>();
  for (const cmd of commands) {
    if (!categories.has(cmd.category)) categories.set(cmd.category, []);
    categories.get(cmd.category)!.push(cmd);
  }
  const sortedCats = [...categories.keys()].sort();
  let html = `<h1>Commands</h1>\n<div class="search-wrapper">\n  <svg class="search-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>\n  <input type="text" class="search-input" id="command-search" placeholder="Filter commands..." autocomplete="off">\n  <button class="search-clear" id="search-clear" type="button">&times;</button>\n</div>\n<p class="search-results-count" id="search-count"></p>\n`;
  for (const cat of sortedCats) {
    html += `<h2 id="cat-${cat.toLowerCase().replace(/\s+/g, "-")}">${cat}</h2>\n`;
    for (const cmd of categories.get(cat)!) {
      html += `<div class="command-entry" data-command="${cmd.name}" data-category="${cmd.category}">\n${cmd.html}</div>\n`;
    }
  }
  html += `<script>(function(){const i=document.getElementById('command-search'),c=document.getElementById('search-clear'),n=document.getElementById('search-count'),e=document.querySelectorAll('.command-entry'),h=document.querySelectorAll('h2[id^="cat-"]');function f(){const q=i.value.toLowerCase().trim();let v=0;e.forEach(function(x){const nm=x.getAttribute('data-command')||'',ct=x.getAttribute('data-category')||'',t=x.textContent||'',m=!q||nm.includes(q)||ct.toLowerCase().includes(q)||t.toLowerCase().includes(q);x.style.display=m?'':'none';if(m)v++});h.forEach(function(x){let s=x.nextElementSibling,ok=false;while(s&&!s.matches('h2')){if(s.classList.contains('command-entry')&&s.style.display!=='none'){ok=true;break}s=s.nextElementSibling}x.style.display=ok?'':'none'});n.textContent=q?v+' command'+(v!==1?'s':'')+' found':''}i.addEventListener('input',f);c.addEventListener('click',function(){i.value='';f();i.focus()})})();<\/script>`;
  return html;
}

async function build() {
  const startTime = performance.now();
  console.log("Building repartee documentation...\n");
  const template = await Bun.file(TEMPLATE_PATH).text();
  const { mkdir } = await import("node:fs/promises");
  await mkdir(join(SITE_DIR, "css"), { recursive: true });
  await mkdir(join(SITE_DIR, "images"), { recursive: true });
  await mkdir(join(SITE_DIR, "js"), { recursive: true });
  const cssSrc = Bun.file(CSS_SRC);
  if (await cssSrc.exists()) { await Bun.write(join(SITE_DIR, "css/style.css"), cssSrc); console.log("  css/style.css"); }
  const jsGlob = new Bun.Glob("*.js");
  try { for await (const js of jsGlob.scan(JS_SRC_DIR)) { await Bun.write(join(SITE_DIR, "js", js), Bun.file(join(JS_SRC_DIR, js))); console.log(`  js/${js}`); } } catch {}
  let pageCount = 0;
  for (let i = 0; i < siteMap.length; i++) {
    const entry = siteMap[i]!;
    let contentHtml: string;
    if (entry.slug === "commands") { contentHtml = await buildCommandsPage(); }
    else {
      const srcPath = entry.source || join(CONTENT_DIR, `${entry.slug}.md`);
      const md = await readText(srcPath);
      if (!md) { console.log(`  [skip] ${entry.slug}.md — not found`); continue; }
      contentHtml = await marked.parse(md);
    }
    const nav = buildNav(entry.slug);
    const { prev, next } = buildPrevNext(i);
    const page = template.replace("{{title}}", entry.title).replace("{{nav}}", nav).replace("{{content}}", contentHtml).replace("{{prev}}", prev).replace("{{next}}", next);
    await Bun.write(join(SITE_DIR, `${entry.slug}.html`), page);
    console.log(`  ${entry.slug}.html`);
    pageCount++;
  }
  console.log(`\nDone — ${pageCount} pages built in ${(performance.now() - startTime).toFixed(0)}ms`);
}

build().catch((err) => { console.error("Build failed:", err); process.exit(1); });
