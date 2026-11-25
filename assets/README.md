# Moonfish — logo asset pack

The mark is the **Opah Lunar Disc** — a circular moonfish emblem split by a crisp lunar crescent channel into an illuminated Moonfish gold leading face and a sculpted opah body with a forked tail.

## Colors

| Name       | Hex       | Use                                                                    |
|------------|-----------|------------------------------------------------------------------------|
| Gold       | `#F5B800` | The brand color: the lunar face (on dark and light), gold icon tile, and the app's one accent |
| Ink        | `#0E0E0F` | Opah body on light, monochrome mark on gold, and dark icon tile        |
| Light      | `#E6EDF3` | Opah body and wordmark on dark (`#EDEDED` in the app UI)               |
| Soft black | `#0C0C0C` | The app's dark background (sidebar `#080808`, cards `#151515`)         |
| Paper      | `#F6F7F9` | The app's light background (sidebar `#EEF0F3`)                         |

Wordmark: JetBrains Mono ExtraBold.

## Files

- `svg/` — master vectors: marks, app icons, lockups
- `png/mark`, `png/icon` — 64–1024 px; `png/lockup` — 600/1200/2400 px wide
- `favicon/` — `favicon.ico` (16/32/48), `favicon.svg`, `apple-touch-icon.png`, `icon-192.png`, `icon-512.png`
- `social/` — GitHub social preview (1280×640) and README banners (dark/light, 1600×400)

## Rules

- Use `mark-on-dark` on dark, `mark-on-light` on light, `mark-black` / `mark-white` / `mark-yellow` for one-color.
- The gold is the same everywhere: logo, app icon, and the app's accents in both themes.
- In the app, gold marks actions and "you are here" only (primary buttons, the selected row,
  selected toggles, badges). Text is never gold: code, commands and links use the text colors.
- Keep clear space around the mark of at least 15% of its width.
- Don't rotate, stretch, close the lunar channel, or remove the eye cutout.

## README header

```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/social/moonfish-readme-banner-dark.png">
  <img alt="Moonfish" src="assets/social/moonfish-readme-banner-light.png">
</picture>
```

## Web

```html
<link rel="icon" href="/favicon.ico" sizes="any">
<link rel="icon" href="/favicon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="/apple-touch-icon.png">
```
