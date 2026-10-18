---
name: product-docs
description: "Keep the Faz website (kaku.fun, the vercel-branch worktree) accurate and user-facing: gather verified features from the current commit, refresh the docs and the plain-language Guide, keep EN/ZH parity, bump version pointers on release, and verify before handing the diff to the maintainer."
when_to_use: "update website, update docs, 更新官网, 完善官网, 产品说明, 使用文档, 上手指南, product guide, document a feature, refresh kaku.fun, write user docs, site docs, 写文档, 写官网"
---

# Faz Product Docs

Use this skill to write or update the user-facing product documentation on the Faz website. The site is the deliverable; this skill is the repeatable flow for keeping it correct and adding the next feature without drift.

The audience is normal users, not contributors. Explain what each page/button/shortcut **is**, how to **reach** it, and what happens **after** you use it. Avoid implementation detail. No em dashes, no emoji (project + global rules).

## Where things live

- **Site worktree**: `~/www/faz-site` on the `vercel` branch (NOT `main`). Vercel serves `kaku.fun` by pushing that branch. `git worktree list` confirms the path.
- **Design system**: Kami parchment system. Read `~/www/faz-site/DESIGN.md` before any visual change. Match the existing pages; do not invent new components.
- **Doc pages** (EN under `docs/`, ZH under `zh/docs/`, kept in lockstep):
  Install (`index.html`), Guide (`guide.html`), Features (`features.html`), CLI Reference (`cli.html`), Configuration (`configuration.html`), Keybindings (`keybindings.html`), FAQ (`faq.html`), Contributing (`contributing.html`).
- **Guide vs Features**: Guide is the narrative onboarding walkthrough (first launch → tabs/panes → shell → AI → tools → settings). Features is feature-by-feature reference. Keep the Guide short and link out to the reference pages; do not duplicate config tables there.
- **Screenshots**: `shots/faz-dark.webp` and `shots/faz-light.webp` (1920x1192). Reuse them with `<figure class="shot">`. Faz is a native terminal, so CDP/browser screenshots do not apply to the app; only capture new app shots if the maintainer explicitly asks (build `make app` or use `/Applications/Faz.app`, then `screencapture`).

## Source of truth: verify, never infer

User-facing docs are public. Do not copy feature claims from a subagent summary or from a feature's name. Confirm each claim against the running code before publishing:

- Defaults and behavior flips: grep `config/src/config.rs` (and its tests, e.g. `*_defaults_to_*`) and bundled config in `assets/`. Example caught this way: v23 flipped `smart_tab_mode` default to `suggestion_first`; the site still said `completion_first`.
- Shell behavior: `assets/shell-integration/setup_zsh.sh`.
- Keybindings / menus: the existing `keybindings.html` is the maintainer's authored ground truth; reuse its wording rather than re-deriving.
- The existing site docs are authoritative for tone and naming. When you add something new, verify it; when you echo something existing, match it.

## Workflow

1. **Scope**: read `git log`/`gh release list` for the current version and what changed. Read the nearest crate `AGENTS.md` and `CLAUDE.md` for feature notes (e.g. `config_version` changes).
2. **Inventory the site**: read the target page(s) raw to clone exact markup. Note `site-nav`, `docs-sidebar > docs-menu`, `section-toc`, `doc-content`, `doc-pager`, `footer`. ZH footer/skip-link wording differs from EN; copy the ZH variants verbatim from an existing ZH page.
3. **Write/edit content** in plain language. New page: clone an existing doc page's full scaffold, change `<title>`/`description`/`canonical`/`hreflang`/hero/`docs-menu` current item/`section-toc`/`doc-pager`. Anchors use a `page-` prefix (`guide-...`).
4. **Wire navigation** (a new page touches every doc page):
   - Insert the new `<a href="/docs/<page>"><span>Label</span></a>` into the `docs-menu` block of all EN doc pages, and the `/zh/docs/...` variant into all ZH pages. The page where the neighbor link carries `aria-current="page"` needs a separate sed (the attribute breaks a naive anchor match).
   - Fix the `doc-pager` prev/next chain on the two neighbor pages only (scope per-file so you do not rewrite the new page's own pager).
   - Add both URLs to `sitemap.xml` and a line to `llms.txt` (and `llms-full.txt` if it enumerates pages).
5. **Version pointers** (only when a release shipped): bump the current-version pointer, not historical references.
   - Bump: nav `>v0.X.Y</a>` (every page), JSON-LD `"softwareVersion"` (both `index.html`), AppleScript `get version` example, `llms-full.txt` `Latest version:`.
   - Leave alone: roadmap narrative ("V0.X.0 is out", "after V0.X.0") and any "this version added" history.
   - Confirm the release is actually public first (`gh release list`), or the site will document an unreleased default.
6. **Verify before declaring done**:
   - `python3 scripts/highlight.py --check` (0 files need highlighting).
   - Internal links resolve under `cleanUrls` (every `/docs/x` has `docs/x.html`).
   - HTML well-formed (no unclosed/stray tags) on new/edited pages.
   - EN and ZH parity: same sections, same anchors, same tables.
   - **Browser screenshot of key pages (CDP / browser-debug)**: capture each changed/new page rendered in a real browser engine. The Faz app is a native terminal, so app screenshots need `make app` / `/Applications/Faz.app` + `screencapture` and are only done on explicit request; the website pages, however, are browser-debuggable and should be screenshotted as standard verification. On a machine with only Safari (no Chromium), use the Preview MCP's bundled engine:
     - Build a temp root so a deep page renders at `/`: `cp docs/<page>.html /tmp/prev/index.html` and symlink the assets it references by absolute path (`ln -sf <site>/styles.css <site>/shots <site>/img /tmp/prev/`).
     - Add a `.claude/launch.json` config running `python3 -m http.server <port> --directory /tmp/prev`, then `preview_start` → `preview_screenshot`; `preview_resize` to `mobile` for the 375px check (DESIGN.md mandates desktop + 375px).
     - The bundled browser caches the loaded page, so after swapping the temp `index.html` to the next page (e.g. the ZH variant), `preview_stop` then `preview_start` to force a fresh load before the next screenshot. Confirm the live nav version, the current-page sidebar highlight, and EN/ZH typography all render.
7. **Hand off**: show the diff. Do NOT commit or push the `vercel` branch unless the maintainer says so this turn (git safety rules). Pushing `vercel` deploys to production immediately.

## Adding the next feature later

- Decide the home: a daily-use behavior → a bullet/step in the **Guide**; a configurable surface or new tool → a section in **Features** (+ Keybindings/Configuration if it adds a shortcut or option).
- Always update EN and ZH together. Mirror anchors and tables exactly.
- Re-run the verify checklist. Keep diffs minimal and atomic per behavior.
