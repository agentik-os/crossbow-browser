# Crossbow Design

## Direction

**Glass over living colour.** A native browser with Arc's organising model and a distinct Crossbow identity. The space colour is the environment; translucent controls sit above it; web content remains the focal point. No borrowed Arc mark, wordmark or proprietary assets.

This is an operating interface, not a marketing page. Precision, familiar controls and recoverable workflows take priority over decoration. Apple's translucent materials are the aesthetic reference, not a claim that custom GPU drawing is the system's Liquid Glass API.

## Surfaces

- Onboarding requests native desktop-backdrop material with a quiet space tint and texture; platforms/accessibility states without material use the opaque space gradient. Setup sheets remain custom translucent surfaces, not live web-content blur. Welcome keeps the application icon, title, short description and one primary action, with no secondary slogan or idle logo animation.
- One elevation per surface. The onboarding sheet uses one offset shadow and a light-catching upper edge, not a border plus a shadow.
- Content is opaque. Chrome may be translucent; readable text and visible focus must not depend on the wallpaper or page underneath.
- Shared radii: 8 pt small controls, 12 pt fields and selectable tiles, 16 pt menus, 26 pt onboarding sheets. Capsule buttons have radius exactly half their height.
- Colour swatches and selection rings are concentric circles. Never draw a square fill inside a rounded selection outline.

## Native desktop transparency

Crossbow's direction includes a **transparent-window material**, in addition to the Arc organizing model. The effect must be visible with the app in an ordinary floating window: the real desktop behind the sidebar and chrome contributes to the material. A translucent rectangle over an opaque in-app gradient does **not** satisfy this requirement.

- Use a shared native material for the window frame, sidebar and utility chrome; avoid independently tuned panel fills. Keep Crossbow's own mark, space colors and interaction model.
- Keep Chromium page surfaces opaque. Browser controls, permission prompts and security origins must remain readable independently of whatever is behind the window.
- Aim for a quiet tint, soft blur, the shared radii, compact type and shape-matched focus. Menus and sheets need an actual composited backdrop, not an imitation claiming to blur desktop or web content that it never samples.
- Expose transparency and tint controls in Appearance. Persist them, restore them on launch, and respect the platform's Reduce Transparency / increased-contrast setting with an opaque fallback. Never simulate the effect by lowering the opacity of the entire window, which would also fade pages and text.
- Native verification must compare normal and 640×420 windows over bright, dark and patterned **synthetic** desktop backgrounds; active/inactive windows; full-screen enter/exit; menus/popups; multiple displays/scales when available; restart; and the opaque fallback. Capture the actual composited screen region as well as the isolated window, since a window-only screenshot can omit the desktop backdrop.
- macOS material behavior is the first milestone. Windows and Linux require explicit platform implementations/fallbacks and their own native verification; the macOS result must not be presented as cross-platform proof.

**Source status:** the native helper now installs a public, mouse-inert `NSVisualEffectView` below the existing render view without reparenting it or lowering window alpha. Appearance persists a device-local transparent/tint draft only on successful Apply; setup requests material temporarily. Unsupported platforms, inaccessible hierarchies and accessibility overrides retain an opaque fallback. The compositor integration and native recovery/geometry paths are being qualified. **No actual desktop-compositing or installed-delivery pass is claimed.** Existing `glass::sheet` is still custom in-app translucency, not a native Liquid Glass API.

**Private-API techniques:** some apps combine a private WindowServer blur with an `NSVisualEffectView` backing. Crossbow must evaluate public API support and platform fallbacks before adopting any private-API technique; neither source inspection nor setting `transparent: true` proves the finished effect.

## Typography

**Inter is the default interface face on every platform**, bundled locally under its SIL Open Font License. Use real variable `wght` and `opsz` coordinates, not egui's colour-only strong style. Medium is the default for controls, tabs and labels; regular is reserved for longer prose. Do not apply SF-specific tracking to Inter or redistribute Apple fonts.

| Role | Size | Weight |
| --- | --- | --- |
| Welcome | 44 pt, 35 pt compact | 600 |
| Setup heading | 32 pt, 28 pt compact | 600 |
| Primary control | 15 pt | 600 |
| Body | 15–17 pt | 400 |
| Control label / tabs | 13–14 pt | 500 |
| Supporting text | 12–14 pt | 400–500 |

Use the font's own metrics and optical sizing; keep body tracking neutral. Text contrast targets 4.5:1; calculate secondary ink from the surface rather than lowering text opacity indiscriminately.

Appearance must offer a Google Fonts family picker with preview, Apply and Reset to Inter. Download only on explicit selection, cache validated font bytes locally, retain license attribution, and fall back safely when offline or a font is unavailable. Font changes affect browser chrome, not arbitrary website typography. Preserve keyboard focus, scroll position and layout at normal and compact sizes; persist the selected family and verify after reopening. The picker is planned until that whole journey has been exercised.

## Iconography and alignment

- Use the licensed **Lucide** icon set throughout browser chrome, setup, settings and menus. Keep platform-native window controls native and the Crossbow brand mark distinct. Do not approximate Lucide with unrelated custom paths or Unicode symbols; user-selected space emoji and website favicons are content, not interface icons.
- Navigation, sidebar toggle and native window controls share a measured vertical center, including hidden-sidebar and compact states. Reserve native-control space only where those controls actually exist.
- Loading indicators must stay inside the content card's rounded silhouette at every progress value and display scale. A rectangular clip alone does not satisfy rounded clipping.
- Use shared label/field spacing and row metrics. Focus and hover follow the actual control shape. Avoid doubled borders/shadows and halos; elevation uses one quiet, offset shadow.
- Compare the real rendered controls at normal and 640×420 sizes, not only source constants. Record remaining mismatches as unfinished work.
- The command field and result rows share icon and text columns (28 pt / 48 pt from the surface edge), with a 56 pt field, 44 pt result rows and 8 pt list insets. Placeholder and entered text use the same 18 pt font and actual measured vertical center. Compact results scroll inside the bounded surface; keyboard navigation reveals the selected row immediately.
- The sidebar URL field is one line, vertically centered using its full font metrics, with ellipsis rather than wrapping. Its leading icon/text align with tab rows. Selection and caret use the shared semantic accent. Settings and the command bar must not remain stacked or let keyboard focus activate browser controls behind them.

## Native refinement gates

Hallmark 2.0.0 guidance is used alongside Impeccable and Emil Kowalski. Apply its app-screen guidance within this locked system: predictable density, readable states, immediate keyboard feedback and precise controls. It does not replace the palette, Inter/Lucide, native window controls or the requirements above. Marketing scaffolds, theme draws, HTML/CSS lint and generated browser mockups do not qualify this native Rust/egui application.

- Selected search results use the shared surface-aware `Ink` accent and `on_accent` for both title and subtitle. Calculate contrast against the actual selected fill, not the panel behind it; preserve a minimum 4.5:1 across built-in light/dark Space presets.
- Favorites use four columns, 44 pt tiles and 6 pt gaps. Reserve at most two complete rows, within a 35% sidebar-body budget, followed by a 14 pt section gap. Overflow scrolls inside that region instead of displacing tabs or the Space switcher.
- Register all favorite controls for keyboard traversal, including clipped rows. Newly focused tiles scroll into view without animation; focus rings match the tile. Clip painting and pointer targets to the favorites viewport, and avoid decoding offscreen favicons.
- Verify search and favorites from their real native entry points at normal and compact sizes, with long results and overflowing favorites, pointer/wheel, Tab/Shift+Tab, Return, cancel and persistence. Preserve open, drag and context-menu actions. Accessibility metadata and simulated frames supplement, but never replace, native verification.

## Application icon

The current direction is a **flat black icon body with a crisp white Crossbow mark**, without gradients. Retain restrained edge highlights and shallow relief only where they improve legibility; no colored glow, busy texture or recurring animation. Render from shared source at all required bundle/Dock sizes so the mark stays balanced and recognizable at small scales.

Settings must offer a preview, built-in variants, a user-selected validated image and Reset. Store the selection and an app-owned image copy locally, never in account sync. Keep the existing icon on cancel, invalid images or failed saves; restore the selection on relaunch. Changes must not repeatedly decode images or update the native icon during rendering.

On macOS, distinguish the installed bundle's Finder icon from a customized runtime Dock/application-switcher icon. Never mutate a working signed bundle to personalize it. Native UI, actual Dock/switcher display, compact keyboard/focus/error states and quit/relaunch persistence are acceptance checks; source assets or a preview alone are not delivery.

## Controls

`app/src/ui/style.rs` owns capsule buttons, focus rings, fields, code input, segmented choices, sliders, circular swatches and keycaps. Reuse these rather than making each step a separate design.

- Primary is ink-filled; secondary is quiet and translucent. One primary action per decision.
- Minimum large-button height: 44 pt. Focus rings follow the control's shape.
- Sliders track pointer movement directly; keyboard arrows change their values.
- Errors explain recovery. Loading blocks duplicate submissions; optional setup must never be mistaken for a successful sign-in or system-default change.
- The built-in egui colour picker is used for custom gradient stops; this is not the operating system's colour panel. A dial-based colour editor is not implemented yet.

## Setup workflow

Welcome → Account (email and code, or local mode) → Account confirmation when signed in → Space → Favorites → Import → Default browser → Shortcuts → Ready.

- Back follows a history stack, including the local-only branch. Escape and Command-[ are back actions.
- Enter submits the active form; it never skips the account form. Keyboard-focused controls own their activation.
- Save the step and path locally. Never save a one-time code in the setup checkpoint.
- Save themes and favorites through the actual library. Reopening must read back those values.
- Import must state its exact scope and exclusions. Source browser files are read-only.
- Opening system settings does not prove that the default changed. No default-browser setting is changed during verification.
- The shortcut exercise is a sandbox: it teaches the keys without changing tabs.
- The final action persists completion before dismissing setup. A failed write leaves an actionable error rather than false success.

## Motion

- Entrances: 220 ms, `cubic-bezier(0.23, 1, 0.32, 1)`.
- Exits: 150 ms. Large spatial transitions: no more than 280 ms.
- Button press: 0.97 scale, with a stable critically damped spring on release.
- Keyboard navigation is immediate. No keyboard command palette entrance animation.
- No bounce, scale-from-zero or continuous decorative animation. Reduced motion removes displacement.

## Verification

Inspect the actual macOS bundle at its normal size and at 640×420. Test forward and backward paths, focus traversal, field submission, color popups, scrolling, persistence, local-only setup and development-server sign-in. Do not confuse a passing build with a completed user journey.

Windows and Linux require their own native verification. A source-level platform branch is not evidence that it runs on that platform.
