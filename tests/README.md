# Verification

## Quality gate

```bash
bash tests/check.sh
bash tests/check.sh --full
bash tests/check.sh --load
```

Both offline modes check Rust formatting, JS/shell syntax, strict Clippy, workspace tests, and all eight combinations of the three optional Extensions. They then rebuild the default binary and run the process-level tests, all against temporary data directories with fake credentials and loopback servers only:

- `tests/http-redirects.mjs` — credential isolation and redirect transparency across all three native protocol surfaces (PROXY-43 / PROXY-44).
- `tests/activity-recovery.mjs` — a torn final Activity record is quarantined at startup while mid-file corruption fails startup with the file and line (ACTIVITY-52).
- `tests/client-disconnect.mjs` — a caller that disconnects mid-stream still produces one explained Activity record, completes its observers, and returns capture quota (PROXY-45).
- `tests/shutdown.mjs` — `SIGTERM` is bounded by the grace period and the request it interrupts is recorded as a shutdown and flushed before exit (SVC-60).
- `tests/response-limits.mjs` — conversion bounds reject an oversized answer with an explained `502`, while native passthrough stays byte-transparent for a response the observer must skip (PROXY-46).
- `tests/openapi-paths.mjs` — every route and method served by the router is documented in `web/openapi.json`, and nothing else is.

The gate needs Rust/Cargo and Node.js; `cargo audit` runs when it is installed, and Cargo may need network access to fetch uncached dependencies. It does not use the running installation's data or `.env`.

`--load` adds `tests/load.mjs`, a capacity baseline that reports p50/p95/p99 latency, throughput, and gateway resident memory at increasing concurrency, then asserts that Activity accounts for every request in the run. It builds the release binary and needs about a minute, so it is not part of the offline gate.

`--full` additionally runs `tests/e2e.sh`, which needs Python 3, jq, curl, Playwright, system Chrome, and Playwright WebKit. This existing suite also calls Cloudflare's real Turnstile test service, so it is not an offline test. It checks API behavior, restart/recovery, and authenticated and logged-out UI paths. A supplied but invalid browser session is a failure, not permission to skip authenticated coverage; uncaught page exceptions also fail browser verification.

`tests/e2e.sh` is not replaced by `check.sh`: it remains the standalone API, browser, and authentication E2E suite, and `bash tests/check.sh --full` is only "offline gate first, then that same suite". Run it on its own (`bash tests/e2e.sh`, optionally with a binary path) whenever the full gate is too heavy, but UI- or auth-visible changes still need it before they are considered verified.

Run a process-level test alone with `node tests/http-redirects.mjs` (rebuilds first), or pass an explicit freshly built binary path to test that artifact. No dependency installation or browser download is performed by the quality gate.

Failing `tests/e2e.sh` runs keep their temporary directory, logs, and data when `YABANE_E2E_KEEP=1` is set.

`bash tests/behavior-map.sh` reports how many rules in `GATEWAY_BEHAVIORS.md` are named by a test, by the source, or by the README, and lists the rest. Naming the rule an automated test covers in a test comment is the convention; the report is informational and never fails a build.

See `ENGINEERING_REVIEW.md` for the engineering findings, validation limits, and prioritized follow-up work.

## UI verification

The admin console must be checked at desktop, tablet, and mobile/iOS-sized viewports whenever layouts, dialogs, forms, navigation, or embedded assets change.

### Automated responsive smoke test

The script uses Playwright with the system Chrome and an isolated browser profile. It does not reuse a personal Chrome profile.

```bash
npm ci
npx playwright install webkit
npm run test:responsive
```

Set `YABANE_UI_BASE` when Yabane is not running at `http://127.0.0.1:8080`. Set `YABANE_SESSION_COOKIE` to an active administrator session value for complete Help, About, and model-route dialog coverage. Against a logged-out console the script still verifies the responsive login layout. `tests/e2e.sh` supplies its temporary administrator session and runs this check automatically; when invoked without a binary path, it rebuilds Yabane first so the embedded assets under test match the working tree.

Profiling identified the authenticated browser suite as the largest E2E stage. After correcting its waits, a local full E2E run with a prebuilt debug binary took 79.45 seconds versus an earlier 103-second baseline; this is not a runtime guarantee. For UI iteration, run `npm run test:responsive` against a freshly rebuilt server with a valid `YABANE_SESSION_COOKIE` (without it, login-only checks are not authenticated coverage).

Live-refresh checks read the interval from the running page, not local source files. Console view scans use `tests/console-view-helper.mjs` to observe the loaders actually invoked by navigation, await their load/render promises (including the nested Activity request-page load), and check the selected view and expected DOM. This is a white-box readiness contract: changes to the console's loading chain must update the helper. It does not equate fetch completion or two animation frames with rendered content. `node tests/console-view-readiness.mjs` checks this helper against a real HTTP response with a held JSON body, nested loading, missing navigation triggers, rejection, and timeout in both Chrome and WebKit. `tests/e2e.sh` runs this regression automatically.

Covered viewports:

- 1440×900 desktop in system Chrome
- 768×1024 tablet in system Chrome
- 375×667 iPhone SE-sized viewport in WebKit (Safari engine)
- 430×932 iPhone Pro Max-sized viewport in WebKit (Safari engine)

The smoke test rejects page-level horizontal overflow, dialogs outside the visual viewport, and undersized dialog close targets. It covers the Activity data-management tabs as well as Help, About, and routing. It also verifies that route traffic shares stay hidden for a simple alias, become a 50/50 percentage split only after explicit opt-in, and prevent saving an invalid total. Route rows carrying long public model names must keep their destination summary inside the table viewport, so the check runs against real routes whose prefixes are longer than an ordinary alias. The Activity Model analysis card is checked for both grouping choices: it starts on the caller's requested name, relabels its column and explanatory copy when switched to the model ID sent to the Provider, requests statistics with the matching `model_dimension`, and keeps the control inside its card on mobile. The Model pricing editor is checked for both price targets in the order an author thinks in: it asks which of the two model names a price matches before where the rule applies, keeps prices for the incoming name Global with the reason visible on screen, shows where a route sends a caller-facing name, and marks caller-facing rules as `Incoming` in the central list. The Activity interval details are checked as a table, not a loose row of values: all ten metrics must land in full rows with their own row and column separators at every viewport width. Every authenticated console view, the API docs page — including every string in the OpenAPI prose it renders — and the logged-out login screen are also checked for the ambiguous word "upstream": directions are named by role (`Client` inbound, `Provider` outbound) and by position in one request (`Incoming model`, `Outgoing model`), so any text node, tooltip, or placeholder containing that word fails the run. Browser verification must happen after rebuilding Yabane because web resources are embedded in the binary.
