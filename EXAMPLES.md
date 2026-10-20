# Worked examples: Lovense games in the wild

Three games read as design references on 2026-10-20 (the two
Lovense game pages via browser; the itch.io page directly). Each
entry records what the page actually states, what STROBE can take
from it, and what the page does not show. Pages describe
marketing, not implementations: none of the three describes what
drives toy output in game, so none is evidence about sync design.

## 1. Projekt Melody: A nut between Worlds!

Source: https://www.lovense.com/game/projekt-melody

What the page states:

- A 3D sci-fi dating game with roleplay elements, first-person
  hack-and-slash combat, 10 dateable characters, a 10-chapter
  story, and a sandbox mode. Early alpha, distributed through the
  developer's Patreon.
- Lovense support is a headline feature. Compatibility is stated
  as "Sync any Lovense Toy"; a recommended-toys carousel shows
  Domi 2, Max 2, Nora, and Solace Pro.
- The connection flow is the Game Mode LAN route, exactly as in
  our bridge contract: sync the toy in Lovense Remote, open
  Discover, Game Mode, enable LAN, read the Local IP and Port,
  enter them in the game's Lovense settings, press connect.
- No rhythm, beat, or music mechanics are mentioned anywhere.
  No platform, price, or system requirements are stated.

What STROBE takes: the connection UX is the shipped-games
standard, and it matches STROBE's config flow (user-entered host
and port) step for step. The landing-page shape (toy support as
the hero claim, "any toy" compatibility, a short recommended
list) is the model for a future STROBE listing page.

What it does not show: any sync behavior. The rhythm-mapped
output remains STROBE's own design; this example validates the
connection layer only.

## 2. Iced Coffee with Breastmilk

Source: https://www.lovense.com/game/iced-coffee-with-breastmilk

What the page states:

- A standalone mini prequel: a cafe game about a barista,
  customizable drinks, multiple endings, and 3D character
  progression. No developer, genre label, platform, price, or
  download route for the game itself appears on the page.
- Compatibility is stated as "Works with: any toy." The
  connection steps are the same generic Game Mode LAN route
  (Remote app, Discover, Game Mode, LAN, paste Local IP and Port
  into the game's Lovense settings). The page also notes toys can
  connect to PC via the Lovense USB Bluetooth adapter.
- The only music mentions concern the Lovense Remote app's own
  features (music syncing, sound-activated play), not this game.
  The game is not described as rhythm or beat based.

What STROBE takes: a second confirmation of the same two
patterns: "any toy" positioning and the identical connection
flow. Also a scope lesson: a small, single-character,
replayable mini-game is a viable Lovense title shape, closer to
a character episode than to a full campaign.

What it does not show: again, no in-game sync behavior of any
kind.

## 3. CumPartment (MaxSUS Games)

Source: https://maxsusgames.itch.io/cumpartment

What the page states:

- An 18+ game by MaxSUS Games (version 2.0.4 at read time):
  play the landlord of an apartment building, meet tenants,
  fulfill their needs, uncover their secrets. Character roster
  rolls out over time, with more in development.
- Playable in the browser now; Windows, Android, and iOS builds
  listed, with native clients planned at completion. Patrons and
  the public receive the same version; some content is
  patron-only. Updates every 2 to 3 weeks; seasonal events
  (Halloween, Christmas) with exclusive scenes and costumes;
  animated and voiced scenes planned if budget allows.
- Community-led distribution: Discord (with a coupon code),
  Patreon, X. The page discloses that some art is AI-generated
  with human creative direction and finishing.
- The page as published makes no mention of Lovense, toys, or
  rhythm mechanics.

What STROBE takes: this is a distribution and live-ops model,
not an integration reference: browser-first play (zero-install
trial), one build for patrons and public with patron-only
unlocks rather than a forked version, a character roster that
grows on a public cadence, seasonal events, and an upfront AI
art disclosure. Mapped onto STROBE: the cast book's ten
characters are a roster that can ship in waves, and the hypnosis
count progression already provides the unlock spine patrons
would get first.

What it does not show: nothing about devices or rhythm. If the
game has either, it is not on its store page, and STROBE should
not assume it.

## Cross-cutting findings

- Both Lovense-published examples use the identical Game Mode
  LAN connection flow STROBE already implements. That flow is
  the de facto standard UX for Lovense games; keep it.
- Neither Lovense page describes what drives toy output. Public
  pages sell connection, not sync design. The beat and judgement
  mapped envelopes are STROBE's differentiator and must be
  designed and documented from our own work (TESTING.md, the
  bridge contract), not copied.
- "Works with any toy" is the standard compatibility claim;
  STROBE can make it honestly because the bridge speaks the
  Standard API and reads capabilities from GetToys.
