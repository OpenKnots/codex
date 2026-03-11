# Mobile Elephant Icon Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the placeholder mobile app icon with a Codex elephant icon that works in the Tauri iOS build path.

**Architecture:** Generate one canonical 1024x1024 source icon, then derive the generated iOS icon sizes from that source so future Tauri/Xcode runs stay visually consistent.

**Tech Stack:** Tauri iOS, Xcode-generated app icon set, Python Pillow for local asset rendering

---

### Task 1: Replace the canonical app icon

**Files:**
- Create: `docs/superpowers/specs/2026-03-11-mobile-elephant-icon-design.md`
- Create: `docs/superpowers/plans/2026-03-11-mobile-elephant-icon.md`
- Modify: `apps/mobile/src-tauri/app-icon.png`

- [ ] **Step 1: Write the failing test**

Run: `sips -g pixelWidth -g pixelHeight apps/mobile/src-tauri/app-icon.png`
Expected: existing placeholder reports `1x1`

- [ ] **Step 2: Generate the replacement icon**

Render a new 1024x1024 elephant icon with the approved glossy blue-and-black metallic look.

- [ ] **Step 3: Run the verification check**

Run: `sips -g pixelWidth -g pixelHeight apps/mobile/src-tauri/app-icon.png`
Expected: `1024x1024`

### Task 2: Refresh derived iOS icons

**Files:**
- Modify: `apps/mobile/src-tauri/gen/apple/Assets.xcassets/AppIcon.appiconset/*`

- [ ] **Step 1: Regenerate the app-icon set**

Resize the canonical icon into the filenames declared by the generated Xcode icon set.

- [ ] **Step 2: Verify one generated representative**

Run: `sips -g pixelWidth -g pixelHeight apps/mobile/src-tauri/gen/apple/Assets.xcassets/AppIcon.appiconset/AppIcon-512@2x.png`
Expected: `1024x1024`

### Task 3: Validate the app still builds

**Files:**
- Test: `apps/mobile`

- [ ] **Step 1: Run package verification**

Run: `pnpm --filter @openai/codex-mobile build`
Expected: pass

- [ ] **Step 2: Re-run the simulator path if needed**

Run: `pnpm --filter @openai/codex-mobile ios:dev "Codex Remote iPhone 16"`
Expected: app launches with the updated icon
