# Crossbow landing

## Direction contract

**Mode:** Persuade. The audience is people considering a successor to a sidebar-first browser; the single conversion is a genuine early-access waitlist signup.

**Thesis:** A fresh home for the same internet. Demonstrate Spaces in an interactive, clearly labelled interface preview; do not manufacture users, download counts, performance claims or completed platform support.

**Own world:** Inherit Crossbow's glass-over-colour identity: pale lilac and cool white, dark plum ink, precise capsule controls and a bow mark. Manrope provides the website's display voice; Inter is the native app's bundled default. A large browser preview carries the colour, not ornamental feature cards.

**Story:** Identify the browser and its development status immediately, join the waitlist without scrolling, explore the organizing model, then resolve platform/import/account questions. macOS first; Windows and Linux planned.

**First viewport:** Compact brand/navigation; a large left-aligned two-line headline with short product copy; the signup form sits alongside as the functional early-access surface. A full-width illustrated browser begins directly below and lets visitors switch between sample Spaces. Clearly label all sample browsing content as a design preview.

**Form:** Extend the existing Arc-workflow / modern Apple-glass world. No new identity selection or generated imagery; the page is code-led. The signature interaction is switching sample Spaces, immediately for keyboard users and with a short colour transition otherwise.

**Finish:** the landing is unfinished until it has had a finish review and a verdict, this file is current, and every shipping raster carries its provenance.

## Tokens and behavior

- Ground `#f6f4fb`, paper `#ffffff`, ink `#211c31`, secondary `#625d70`, accent `#6b48c8`; background shade comes from the sample Space.
- Display type: Manrope variable, 580–650 weight, negative tracking no lower than -0.04em. Body 15–18px; desktop headline fluid 52–88px.
- Page maximum 1240px, 40px desktop gutters, 20px mobile gutters. Content must fit a 390px viewport with no horizontal page scroll.
- Controls: 48px minimum primary actions, visible shape-matched focus rings, genuine disabled/busy/error/success states, no hidden loading content.
- One short entrance and restrained hover feedback. `prefers-reduced-motion` removes movement; Space switching by keyboard is immediate. No cursor-following effects or idle animations.
- Preview is illustrative, never a fake screenshot or a claim of tested application features. No remote brand logos or invented endorsements.

## Waitlist truth and privacy

The browser's existing account/sync service remains separate. Registration becomes available only when the dedicated Clerk integration, abuse controls and a verified privacy contact are configured. Failed requests never show success; duplicate registrations receive the same neutral success response. No email is stored in browser storage or written to logs.

The privacy page describes the email purpose and Clerk/Cloudflare processing. A contact address is supplied by configuration, not invented. No analytics, retargeting scripts or marketing counters are part of this landing.

## Current review status

Implementation review and review of the running site in a browser are pending. Do not label this landing visually or end-to-end verified until the running site has been exercised at desktop/mobile sizes and a real Clerk entry has been read back.
