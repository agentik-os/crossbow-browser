# Product

<!-- impeccable:product-schema 1 -->

## Platform

Native desktop app: macOS first, with Windows and Linux targets that keep the same layout and bundled Inter/Lucide system while using their native window controls. Not a web app. Phone and iPad continuity is an explicit future product surface, not proof that the desktop CEF runtime ships on mobile.

## Stack

Rust throughout. Chromium via CEF in off-screen rendering, composited with wgpu; UI drawn with egui/eframe. Account sync on Cloudflare Workers (Rust `crossbow-api`, Durable Objects, D1). AI direction is open, explicitly selected installed providers and local servers; a separate TypeScript Worker (Agents SDK, AI SDK, Workers AI) is legacy infrastructure, not a mandatory browser AI route or a completed feature.

## Users

The public. People who live in their browser all day (makers, researchers, people who switch between work and personal contexts) and want it calmer and more organised than a tab strip. Arc users who need a successor that also runs on Windows and Linux are a primary audience.

## Product Purpose

Crossbow is a full web browser with Arc's way of working: a vertical sidebar instead of tabs, Spaces for separate contexts, pinned tabs that stay put, Today tabs that archive themselves, a command bar for everything, split view, Peek, Boosts, and account sync across macOS, Windows and Linux. Success means an Arc user can switch with no loss of features or habits, and a new user understands the model within the first minute.

## Positioning

Arc-inspired workflows, rebuilt in Rust, with its own visual identity. The target identity provider is Clerk; Cloudflare hosts the browser's sync service. The current custom email-code service is a development implementation to migrate, not the final identity decision. Passwords and browsing-library synchronization require separately designed end-to-end encryption, device enrollment, revocation and recovery. AI remains off until useful, verified features ship. Crossbow does not copy Arc's trade dress (logo, wordmark, icon set or signature colours).

## Operating Context

Used all day, mostly by keyboard (⌘T, ⌘L, ⌘S, ⌘D, space switching), with the mouse for drag-and-drop in the sidebar. Daylight offices and late evenings, so light and dark both matter and each space can have its own colour. Window sizes from a half-screen split to full screen.

## Capabilities and Constraints

- Implemented in source, with what has been checked in the running app listed in [feature status](docs/feature-status.md): sidebar (favorites, pinned tree with folders, Today), spaces with themes and profiles, command bar, split view (up to 4), Peek, find on page, context menus, archive/history/downloads, Boosts (CSS/JS), settings, onboarding, Arc and Chrome import, email-code sign-in and sync. This is not a claim of complete or verified Arc parity.
- Known gaps: no Chrome extensions (off-screen rendering), no proprietary codecs or Widevine in stock CEF, Google sign-in may refuse embedded browsers, popups open as tabs or Peek, Notes/Easels not built yet. Little Windows, Crossbow's version of Little Arc, exist in source; their shortcut, clicks and timer have not been exercised in the running app.
- Open AI provider choice and Dia-inspired context/conversations/actions are additional required scope. Initial discovery/diagnostics are supplemented by source-only, session-only prompt conversations with explicit Settings and command-bar entry; ordinary Search remains the default and entry sends nothing. Exact commands remain reachable among matching tabs; submission and cancellation isolate closing-command input from the page. Successful navigation supersedes queued Find restoration without clearing its query. Settings owns its covered controls, dropdowns and colour picker. These source repairs do not establish native qualification; page context and actions remain unfinished. Installed clients may be cloud-backed; loopback does not prove local inference. Saving a selection is not consent to transmit browsing data.
- Keyboard-initiated actions must be instant; animation is for spatial changes (sidebar, space switch, Peek) and first-run moments.

## Brand Commitments

- Name: Crossbow. Mark: a drawn bow with a nocked arrow (vector, generated in `xtask`).
- English UI copy, plain and short.

## Evidence on Hand

- Arc Settings comparison: [Arc Settings parity](docs/arc-settings-parity.md).
- No testimonials, metrics or press exist; never invent them.

## Product Principles

1. The page is the hero: chrome recedes, content gets the pixels.
2. Keyboard first, and the keyboard never waits for an animation.
3. Calm by default: auto-archive, quiet colour, nothing that blinks for attention.
4. Parity before novelty: an Arc habit should work the same way here.
5. The user's data stays the user's: local-first, sync is opt-in.

## Accessibility & Inclusion

Text contrast at least 4.5:1 on every space theme (themes are user-chosen, so contrast is computed, not assumed). Reduced-motion setting removes movement. Every action reachable by keyboard.
