# Lovense Integration — Nice Thoughts Ranch

Working repository for Nice Thoughts Ranch's Lovense integration:
the studio's presence in the Lovense App Gallery, toy integration
for STROBE, and media-sync research for future interactive media.

This is a planning and integration repo. It contains no credentials
and no production code yet. Code lands here only after the checklist
gates below are passed.

## Start here

- [CHECKLIST.md](CHECKLIST.md): the phased work list, account and
  console setup first, code last.
- [TESTING.md](TESTING.md): the test ladder for the integration,
  mock first, live hardware last and gated.
- [DOCS-NOTES.md](DOCS-NOTES.md): condensed notes from the official
  Lovense developer documentation (read 2026-10-20), including the
  per-plugin capability matrix and the sub-pages still to read.
- [TOYS.md](TOYS.md): the current Lovense developer-program toy
  catalog (24 devices with product pages).

## The Lovense surfaces, and which ones are ours

Lovense publishes several unrelated API families. Keeping them
straight is most of the work:

1. **Standard API (Game Mode, local LAN).** The game talks directly
   to the Lovense Remote app on the player's network. This is
   STROBE's integration route; a gated bridge implementation already
   exists in the STROBE project (master toggle defaults off, output
   clamped in the bridge, stop is first-class). The server route of
   the Standard API (commands relayed through Lovense's cloud with a
   developer token) was evaluated and rejected for STROBE on latency
   and token-custody grounds.
2. **App Gallery platform API.** For apps hosted inside Lovense
   Remote's App Gallery. This is the surface behind the App Gallery
   Console (public profile, app listings). New territory for the
   studio; Phase 0 and Phase 4 of the checklist cover it.
3. **Pattern Editor / Media Sync SDK.** A web JavaScript SDK
   (`pattern-sdk.js`) that syncs toys to web audio/video, with
   FunScript and Lovense pattern support and an AI sync fallback.
   Relevant only if the studio ships web media; tracked as research
   in Phase 5.
4. **Game engine plugins.** Six Unity plugins (Universal Remote,
   Android, iOS, Windows Bluetooth, USB Dongle, Mac), an Unreal
   Remote plugin, a Windows BLE DLL for C/C++/C#, and a Ren'Py
   plugin. The Unity Windows and Dongle plugins are the evaluation
   candidates for STROBE's Windows build; see DOCS-NOTES.md for the
   capability matrix.
5. **Native SDKs.** iOS, Android, and Windows SDKs for standalone
   apps. Not currently planned; noted for completeness.

## Credentials

Developer credentials (token, key, IV) live in the studio password
store under `lovense/ntr/` and are referenced by path only. They are
never committed to this repo, never pasted into issues or chat, and
are used only by the server-side flows that require them.

## Official documentation

- Media Sync / Pattern Editor:
  https://developer.lovense.com/docs/media-sync.html
- Game engine plugins:
  https://developer.lovense.com/docs/game-engine-plugins/
- Native SDKs:
  https://developer.lovense.com/docs/native-sdks.html
- App Gallery Console:
  https://appgallery.lovense.com/remote/console/#/
- Developer dashboard:
  https://www.lovense.com/user/developer/info

## License

Apache-2.0. See [LICENSE](LICENSE).
