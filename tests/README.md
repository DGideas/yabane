# UI verification

The admin console must be checked at desktop, tablet, and mobile/iOS-sized viewports whenever layouts, dialogs, forms, navigation, or embedded assets change.

## Automated responsive smoke test

The script uses Playwright with the system Chrome and an isolated browser profile. It does not reuse a personal Chrome profile.

```bash
npm ci
npx playwright install webkit
npm run test:responsive
```

Set `YABANE_UI_BASE` when Yabane is not running at `http://127.0.0.1:8080`. Set `YABANE_SESSION_COOKIE` to an active administrator session value for complete Help, About, and model-route dialog coverage. Against a logged-out console the script still verifies the responsive login layout. `tests/e2e.sh` supplies its temporary administrator session and runs this check automatically; when invoked without a binary path, it rebuilds Yabane first so the embedded assets under test match the working tree.

Covered viewports:

- 1440×900 desktop in system Chrome
- 768×1024 tablet in system Chrome
- 375×667 iPhone SE-sized viewport in WebKit (Safari engine)
- 430×932 iPhone Pro Max-sized viewport in WebKit (Safari engine)

The smoke test rejects page-level horizontal overflow, dialogs outside the visual viewport, and undersized dialog close targets. It covers the Activity data-management tabs as well as Help, About, and routing. It also verifies that route traffic shares stay hidden for a simple alias, become a 50/50 percentage split only after explicit opt-in, and prevent saving an invalid total. Route rows carrying long public model names must keep their destination summary inside the table viewport, so the check runs against real routes whose prefixes are longer than an ordinary alias. The Activity Model analysis card is checked for both grouping choices: it starts on the caller's requested name, relabels its column and explanatory copy when switched to the model ID sent to the Provider, requests statistics with the matching `model_dimension`, and keeps the control inside its card on mobile. The Model pricing editor is checked for both price targets in the order an author thinks in: it asks which of the two model names a price matches before where the rule applies, keeps prices for the incoming name Global with the reason visible on screen, shows where a route sends a caller-facing name, and marks caller-facing rules as `Incoming` in the central list. The Activity interval details are checked as a table, not a loose row of values: all ten metrics must land in full rows with their own row and column separators at every viewport width. Every authenticated console view, the API docs page — including every string in the OpenAPI prose it renders — and the logged-out login screen are also checked for the ambiguous word "upstream": directions are named by role (`Client` inbound, `Provider` outbound) and by position in one request (`Incoming model`, `Outgoing model`), so any text node, tooltip, or placeholder containing that word fails the run. Browser verification must happen after rebuilding Yabane because web resources are embedded in the binary.
