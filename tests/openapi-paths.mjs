// The embedded API reference must describe the routes and methods the binary
// actually serves, so a route added, renamed, or removed together with the router
// cannot leave the OpenAPI document explaining an API that no longer exists.
import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
// Console pages and embedded assets are not part of the JSON API reference.
const consoleRoutes = new Set([
  '/', '/home', '/login', '/providers', '/providers/{id}', '/model-routing', '/model-pricing',
  '/management-api', '/api-access', '/activity', '/extensions', '/extensions/traffic-capture',
  '/app.css', '/app.js', '/favicon.svg', '/fonts/ubuntu-sans-medium.woff2',
  '/fonts/ubuntu-sans-regular.woff2',
]);
const methods = ['get', 'post', 'patch', 'delete', 'put'];

// Read one `.route(…)` call at a time so each path keeps its own methods.
function routesOf(source) {
  const found = [];
  const marker = '.route(';
  for (let index = source.indexOf(marker); index !== -1; index = source.indexOf(marker, index + 1)) {
    let depth = 1;
    let end = index + marker.length;
    for (; end < source.length && depth > 0; end++) {
      if (source[end] === '(') depth++;
      if (source[end] === ')') depth--;
    }
    const call = source.slice(index + marker.length, end);
    const name = /^\s*"([^"]+)"/.exec(call);
    if (!name) continue;
    const tail = call.slice(call.indexOf(',') + 1);
    found.push([name[1], methods.filter(method => new RegExp(`\\b${method}\\(`).test(tail))]);
  }
  return found;
}

const sources = [];
for (const [directory, name] of [
  [path.join(repo, 'src'), null],
  ...(await readdir(path.join(repo, 'extensions'))).map(name => [path.join(repo, 'extensions', name, 'src'), name]),
]) {
  let entries;
  try { entries = await readdir(directory); } catch { continue; }
  for (const file of entries) {
    if (file.endsWith('.rs')) sources.push(await readFile(path.join(directory, file), 'utf8'));
  }
}

const canonical = route => route.replaceAll(/\{[^}]+\}/g, '{}');
const served = new Map();
for (const source of sources) {
  for (const [route, routeMethods] of routesOf(source)) {
    if (consoleRoutes.has(route)) continue;
    const key = canonical(route);
    const entry = served.get(key) ?? { routes: new Set(), methods: new Set() };
    entry.routes.add(route);
    for (const method of routeMethods) entry.methods.add(method);
    served.set(key, entry);
  }
}

const document = JSON.parse(await readFile(path.join(repo, 'web/openapi.json'), 'utf8'));
const documented = new Map();
for (const [route, operations] of Object.entries(document.paths ?? {})) {
  documented.set(canonical(route), {
    route,
    methods: new Set(methods.filter(method => operations[method])),
  });
}

const problems = [];
for (const [key, entry] of served) {
  const doc = documented.get(key);
  const label = [...entry.routes].sort().join(' or ');
  if (!doc) {
    problems.push(`${label}: served but not documented`);
    continue;
  }
  for (const method of entry.methods) {
    if (!doc.methods.has(method)) problems.push(`${label}: ${method.toUpperCase()} is served but not documented`);
  }
  for (const method of doc.methods) {
    if (!entry.methods.has(method)) problems.push(`${label}: ${method.toUpperCase()} is documented but not served`);
  }
  if (!entry.routes.has(doc.route)) {
    console.warn(`warning: ${label} documents different path parameter names (${doc.route})`);
  }
}
for (const [key, doc] of documented) {
  if (!served.has(key)) problems.push(`${doc.route}: documented but not served`);
}
assert.deepEqual(problems, [], `web/openapi.json no longer describes the router:\n  ${problems.join('\n  ')}`);
console.log(`OpenAPI matches the router: ${served.size} API paths`);
