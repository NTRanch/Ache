# Lovense Integration Checklist

Phased in dependency order. Console and account work comes first,
evaluation second, code last. Items marked [studio] are clicks only
the account holder can make; everything else is preparatory or
review work.

## Phase 0 — Account and console

- [x] Lovense developer account exists and is active.
- [x] Developer credentials (token, key, IV) stored in the studio
      password store at `lovense/ntr/{token, key, iv}` (2026-10-20).
      Paths only in this repo; values never leave the store.
- [ ] [studio] App Gallery Console, Public Profile: set Display Name
      (max 20 characters; "Nice Thoughts Ranch" is 19 and fits).
- [ ] [studio] Public Profile: fill social links as accounts are
      claimed: Discord (discord.gg channel), Instagram (userid),
      X (userid), YouTube (channel, `@NiceThoughtsRanch`).
- [ ] [studio] Public Profile: Save, then view the public app detail
      page once to confirm the profile renders as intended.
- [ ] Confirm which name the developer dashboard shows to users for
      the token (the docs warn the website/app name entered there is
      displayed to end users; enter it carefully).

## Phase 1 — Documentation review

- [x] Read Media Sync / Pattern Editor doc (2026-10-20; notes in
      DOCS-NOTES.md). Note: `media-sync.html` now serves the Pattern
      Editor documentation.
- [x] Read game engine plugins index (2026-10-20; capability matrix
      in DOCS-NOTES.md).
- [x] Read native SDKs landing page (2026-10-20; it is a link hub
      only, no technical content).
- [ ] Read the ctoken guide:
      https://developer.lovense.com/docs/media-sync/get-ctoken.html
- [ ] Read the Unity Windows plugin doc:
      https://developer.lovense.com/docs/game-engine-plugins/unity-plugin-winbluetooth.html
- [ ] Read the Unity Dongle plugin doc:
      https://developer.lovense.com/docs/game-engine-plugins/unity-plugin-dongle.html
- [ ] Read the Unity Universal (Remote) plugin doc, linked from the
      plugins index.
- [ ] Read the three native SDK pages (iOS, Android, Windows) only if
      a standalone app enters the roadmap.
- [ ] Close the open gaps register: developer terms and licensing
      for plugin redistribution, rate limits, and Unity version
      support are not stated in the docs read so far. Do not assume;
      verify or ask Lovense support.

## Phase 2 — Surface decisions (record outcomes in this file)

- [ ] STROBE (Windows, Unity): stay on the Standard API local Game
      Mode bridge, adopt the Unity Windows Bluetooth plugin, adopt
      the Dongle plugin, or layer (bridge for Remote users, plugin
      for direct Bluetooth)? Decision inputs: Phase 3 evaluation.
- [ ] App Gallery: does the studio list an app, and if so what is it
      (companion app, media player, storefront link-out)? The public
      profile (Phase 0) is worthwhile either way; an app listing is
      a product decision, not a formality.
- [ ] Media Sync: adopt only if the studio ships web video/audio.
      FunScript support makes it the natural sync route for
      pre-scripted media; VibeMate AI Sync covers unscripted media.

## Phase 3 — Unity plugin evaluation (only after Phase 2 opens it)

- [ ] Download the Unity Windows plugin and the Dongle plugin from
      their doc pages.
- [ ] Import into a throwaway copy of the game project. Never test a
      new plugin first in the live project tree.
- [ ] Verify against the doc capability matrix: connect/disconnect,
      commands, presets, patterns, button event, shake event,
      shake-times event (all listed as supported for both plugins).
- [ ] Verify engine compatibility with the project's Unity version
      (the docs state no version requirements; treat as unverified
      until the import compiles and a toy answers).
- [ ] Compare against the existing bridge constraints before any
      adoption: master toggle defaults off, intensity clamped in
      our code (not by configuration), synchronous stop on pause,
      menu, focus loss, and panic input, no output resumption on
      reconnect. A plugin that cannot honor these does not ship.
- [ ] Testing against real hardware is live work: it happens only
      on an explicit go from the studio, never as part of a routine
      build check.

## Phase 4 — App Gallery listing (only after Phase 2 decides it)

- [ ] Complete Phase 0 public profile first; listings display it.
- [ ] [studio] Create the app entry in the console Apps section.
- [ ] Prepare listing assets (icon, screenshots, description)
      consistent with the studio profile and the STROBE key art.
- [ ] Build the app against the App Gallery platform API family
      (the surface documented under the developer site's Remote
      section), not the game Standard API.
- [ ] [studio] Submit for review; record review feedback and
      outcomes in this repo.

## Phase 5 — Media Sync pilot (only if Phase 2 adopts it)

- [ ] Developer dashboard configured; ctoken flow working (see the
      ctoken guide, Phase 1).
- [ ] Quick integration on a test page: `pattern-sdk.js`,
      `new LovensePattern()`, one `sync({...})` call with ctoken,
      mediaId, duration, btnId, supportedApp.
- [ ] One FunScript file playing in sync through Lovense Remote
      (supportedApp 1) and through VibeMate (supportedApp 2).
- [ ] Advanced flow only if needed: `init`, `getApp`, `loadPattern`,
      `play` inside the `patternLoaded` event, `pause`/`change`
      wired to the media element, `PatternSyncError` and
      `appDisconnected` handled visibly for the viewer.

## Standing rules

- Credentials are referenced by password-store path only, in every
  repo, issue, and document. No exceptions.
- Studio attribution only on all public surfaces (no personal
  names), matching the rule for every Nice Thoughts Ranch repo.
- Anything user-facing that the developer dashboard displays (app
  name, website name) gets proofread twice before saving; the docs
  warn it is shown to end users exactly as entered.
