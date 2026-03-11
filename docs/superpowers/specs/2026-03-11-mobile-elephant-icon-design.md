# Mobile Elephant Icon Design

## Goal

Replace the placeholder Tauri iOS app icon with a Codex-branded elephant icon that preserves the reference image's glossy blue-and-black metallic feel while remaining legible at small iOS icon sizes.

## Decision

Use a simplified elephant-head mark instead of a literal remake of the reference image. Keep these traits from the reference:

- deep blue glossy center treatment
- black and silver metallic loop forms around the mark
- dark, premium contrast with strong highlights

Change these traits for icon clarity:

- remove the cloud and terminal glyph
- make the elephant silhouette the focal shape
- simplify the surrounding metallic forms so the icon still reads on the home screen

## Asset Scope

- replace `apps/mobile/src-tauri/app-icon.png` with a 1024x1024 opaque PNG
- refresh the generated iOS app-icon set under `apps/mobile/src-tauri/gen/apple/Assets.xcassets/AppIcon.appiconset/`

## Verification

- source icon is no longer `1x1`
- generated iOS app-icon set is refreshed from the new source
- `pnpm --filter @openai/codex-mobile build` still passes
- simulator launch path still works after the icon replacement
