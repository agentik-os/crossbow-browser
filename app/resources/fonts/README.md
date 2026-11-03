# Bundled interface fonts

These local assets do not contact a font provider at runtime.

- **Inter variable**: `Inter.ttf`, from `google/fonts/ofl/inter/Inter[opsz,wght].ttf`. Original Inter Project Authors; SIL Open Font License in `Inter-OFL.txt`. SHA-256: `29160a80ff49ddcab2c97711247e08b1fab27a484a329ce8b813d820dc559031`. Weight and optical-size axes are used directly; no derivative font is generated.
- **Lucide 0.468.0**: `lucide.ttf`, from `lucide-static@0.468.0/font/lucide.ttf` on npm. Glyph mappings in `app/src/ui/icons.rs` match that release's `font/info.json`. Lucide/Feather attribution and ISC license in `Lucide-LICENSE.txt`. SHA-256: `0ff17e4ce16364d2dae72689336262960e7d5067cfcdb07ca283465a2a34af48`.

Keep licenses with distribution resources. Never update an icon font independently of its glyph mapping and rendering tests. The Crossbow mark, native window controls, website favicons and user-selected space emoji are not Lucide glyphs.
