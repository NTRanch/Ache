# Lovense Developer Documentation — Notes

Condensed from the official docs, read 2026-10-20. These are
working notes, not a copy of the documentation; the canonical text
lives at developer.lovense.com. Page "last updated" stamps as shown
on the pages themselves.

## 1. Media Sync is now the Pattern Editor doc

https://developer.lovense.com/docs/media-sync.html
(last updated 2025-08-24)

The URL serves the **Lovense Pattern Editor** documentation: a set
of SDK tools for web-based audio/video platforms. It syncs toys to
on-screen action or audio, supports FunScript files, Lovense
patterns, and "Lovense AI Sync" (vibration matched to on-screen
movement in real time when no script exists). Two playback apps:

- **Lovense Remote**: users likely already have it; media must play
  in a separate browser; scripted video/audio only.
- **VibeMate**: one browser app for connection and playback;
  scripted sync plus AI Sync; built-in pattern creation; users must
  install it separately.

Setup, in the doc's order:

1. Configure the Developer Dashboard and get the developer token.
   The website name entered there is displayed to users.
2. Get a **ctoken** (separate guide:
   /docs/media-sync/get-ctoken.html — not yet read).
3. Embed `https://cdn.lovense-api.com/peditor-v2/pattern-sdk.js`,
   construct `new LovensePattern()`. Initialization must be called
   every time a media page opens or the media switches.

**Quick integration (recommended)**: a single `sync({...})` call.
Required: `ctoken`, `mediaId` (videoId may substitute), `duration`
(ms), `btnId` (container for the sync button), `supportedApp`
(1 = Remote only, 2 = VibeMate only, 3 = both). Optional: `videoEl`,
`patternLink` (FunScript or pattern file URL), `patternData`,
`syncButtonText` / `syncedButtonText`, `isVR`.

Pattern data formats: FunScript entries `{ pos, at }`; Lovense
pattern entries `{ v, t }`.

**Advanced flow** (five steps): `init({ctoken, mediaId, duration,
videoEl?})`; `getApp(callback)` to connect (callback yields qrCode,
appOpenUrl, and a 6-digit code for VR; the VibeMate variant also
yields a per-page download URL); `loadPattern({type, data,
patternLink})` or `loadMultiplePatterns([...])` for per-toy control
(per-toy requires Lovense Remote v7.63.0+; toy IDs come from the
`appToyStatus` event); `play({speed, currentTime})`, called only
inside the `patternLoaded` event; then `pause()` and
`change({speed, currentTime})` to track the media.

Events: `appConnected`, `appDisconnected` (fires after ~85 s without
server contact), `appExitPattern` (intentional disconnect only),
`appToyStatus` (toy list with nickName, name, id, battery, version,
status), `PatternSyncError` (data.msg carries the error string).
Optional API: `getPatternList({mediaId, page, pageSize})`,
`exit()`. A pattern editor panel can also be embedded on desktop
(`editInit`, editor min-width 720 px, one pattern per video).

## 2. Game engine plugins index

https://developer.lovense.com/docs/game-engine-plugins/
(last updated 2025-04-25)

The index lists plugins with a capability matrix. Columns:
Connect/Disconnect, Commands, Preset, Pattern, Button Event, Shake
Event, ShakeTimes Event. Presets named in the doc: earthquake,
fireworks, pulse, wave.

| Plugin | Platform | Connect | Commands | Preset | Pattern | Button | Shake | ShakeTimes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Unity Universal (Remote) | Android/iOS/PC | No | Yes | Yes | Yes | Yes | Yes | Yes |
| Unity Android | Android (Bluetooth) | Yes | Yes | Yes | Yes | No | No | No |
| Unity iOS | iOS (Bluetooth) | Yes | Yes | Yes | Yes | No | No | No |
| Unity Windows | Windows 10 (Bluetooth adapter) | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Unity Dongle | Windows (Lovense USB adapter) | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Unity Mac | OSX (Bluetooth adapter) | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Unreal (Remote) | PC | No | Yes | Yes | Yes | Yes | Yes | Yes |

Also listed: a Windows BLE DLL for C/C++/C# (direct toy connection
and commands), and a Ren'Py plugin for Remote (commands, patterns,
presets).

Notes and caveats:

- The "Remote" plugins depend on the Lovense Remote app running on
  PC or mobile; they cannot connect/disconnect toys themselves.
- The index states **no version numbers, engine requirements,
  download steps, or configuration**; each plugin has its own doc
  page (the checklist links the ones we need). The Mac section's
  body text appears to repeat the Windows wording (an upstream copy
  error); the matrix row is the reliable part.
- The event definitions as printed look swapped (Button Event is
  described with shake wording and vice versa). Verify semantics on
  the per-plugin pages before designing around them.

## 3. Native SDKs landing page

https://developer.lovense.com/docs/native-sdks.html
(last updated 2024-04-07)

A link hub only: "Integrate Lovense features into your app using
one of our SDKs," with three sections (iOS Apps, Android Apps,
Windows Apps) linking to per-platform pages
(/docs/native-sdks/{ios,android,windows}-native-sdk.html). No
requirements or integration steps on the landing page itself. The
per-platform pages are the real documentation and are unread;
they matter only if a standalone app enters the roadmap.

## Open gaps (carried from earlier research, still open)

- Developer terms and licensing, including plugin redistribution in
  a shipped game: not located in the docs read to date.
- Rate limits: not stated on any page read.
- Unity version support for the plugins: not stated.
- Whether the Standard API Game Mode route formally requires a
  developer token: docs observed to describe no token for the local
  route, but never say so explicitly.
