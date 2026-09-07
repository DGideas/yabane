# UI verification

The admin console must be checked at desktop, tablet, and mobile/iOS-sized viewports whenever layouts, dialogs, forms, navigation, or embedded assets change.

## Automated responsive smoke test

The script uses Playwright with the system Chrome and an isolated browser profile. It does not reuse a personal Chrome profile.

```bash
npm ci
npx playwright install webkit
npm run test:responsive
```

Set `YABANE_UI_BASE` when Yabane is not running at `http://127.0.0.1:8080`. Set `YABANE_SESSION_COOKIE` to an active administrator session value for complete Help, About, and model-route dialog coverage. Against a logged-out console the script still verifies the responsive login layout. `tests/e2e.sh` supplies its temporary administrator session and runs this check automatically.

Covered viewports:

- 1440×900 desktop in system Chrome
- 768×1024 tablet in system Chrome
- 375×667 iPhone SE-sized viewport in WebKit (Safari engine)
- 430×932 iPhone Pro Max-sized viewport in WebKit (Safari engine)

The smoke test rejects page-level horizontal overflow, dialogs outside the visual viewport, and undersized dialog close targets. It covers the Activity data-management tabs as well as Help, About, and routing. It also verifies that route traffic shares stay hidden for a simple alias, become a 50/50 percentage split only after explicit opt-in, and prevent saving an invalid total. Browser verification must happen after rebuilding Yabane because web resources are embedded in the binary.
