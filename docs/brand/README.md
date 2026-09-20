# Buildmesh brand

The mark, the wordmark, where each asset is used, and how to regenerate them.

## The mark

**Relay** — an `M` built from a five-node mesh inside a rounded plate, split
two-tone at the centre hub: cyan on the left, emerald on the right, violet on
the hub node.

![Buildmesh mark](../../src/assets/logo.svg)

The split is flat rather than a gradient, deliberately. A gradient spends its
middle in a low-contrast teal that disappears first when the mark shrinks, and
it cannot be printed, embroidered, or embossed. Two flat fields meeting at the
hub stay distinguishable all the way down to favicon sizes.

## Colour

Every value is an app token declared in `src/App.css`. The mark introduces no
colours of its own.

| Token | Dark | Light |
|---|---|---|
| `--color-accent-cyan` | `#00d4ff` | `#0891b2` |
| `--color-accent-green` | `#22c55e` | `#16a34a` |
| `--color-accent-violet` | `#8b5cf6` | `#7c3aed` |
| `--color-bg-surface` | `#111116` | `#ffffff` |
| `--color-bg-card` | `#16161d` | `#f5f5f7` |

The light column exists because `#00d4ff` sits at roughly 1.7:1 on white — fine
for dark surfaces, invisible on light ones. The darkened values are the
smallest step that still reads as the same hue.

## Two marks, by size

| | Asset | Use above | Use |
|---|---|---|---|
| Full mark | `src/assets/logo.svg` | 24px | Badge with ring and circular nodes — app icon, avatars |
| Compact mark | `docs/brand/b3-relay-icon.svg` | 16px | Plate plus heavy square nodes; drops the ring and circular form, which turn to mud when small |

Any new surface should pick the variant that matches its smallest real render
size rather than scaling one mark everywhere.

## Files

Brand sources live in this folder; the app consumes generated copies.

| Path | Role |
|---|---|
| `docs/brand/b3-relay-mark.svg` | Canonical full mark |
| `docs/brand/b3-relay-lockup-dark.svg` | Mark plus wordmark, for dark surfaces |
| `docs/brand/b3-relay-lockup-light.svg` | Mark plus wordmark, for light surfaces |
| `src-tauri/app-icon.svg` | Source for the platform icon set |
| `src/assets/logo.svg` | Compact mark shipped to the app |
| `src/assets/wordmark-on-dark.png` | Title bar (dark theme) and README (dark theme) |
| `src/assets/wordmark-on-light.png` | Title bar (light theme) and README (light theme) |
| `src/assets/logo.png`, `src/assets/apple-touch-icon.png` | Favicon fallback, iOS home screen |
| `mobile/public/` | Favicon and icons for the remote-access SPA |
| `src-tauri/icons/` | Generated Windows, macOS, and store icons |

The `preview.html` and `preview-nexus.html` files in this folder are the review
sheets the mark was chosen from. They are design history, not current truth.

## Regenerating

```sh
npm run brand:build                              # SVG sources -> PNG rasters
npx tauri icon src-tauri/app-icon.svg            # PNG -> .ico, .icns, store logos
```

`brand:build` rasterises with Playwright's Chromium and loads the same Google
Fonts request `index.html` uses, so the wordmark text matches what the app
renders. It therefore needs network access; the resulting PNGs are committed so
ordinary builds never depend on it.

`tauri icon` writes the whole platform set plus Android and iOS folders. This
project has no Tauri mobile targets — its "mobile" is a web SPA served by the
embedded HTTP server — so delete the generated `icons/android/` and
`icons/ios/` directories after running it.

## Adding a surface

1. Use a token value, or the light counterpart, never an interpolated shade.
2. Pick the mark variant by the smallest size it will render at.
3. If it is a raster, add a job to `scripts/generate-brand-assets.mjs` rather
   than exporting a file by hand.
