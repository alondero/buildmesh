# PR screenshots

Before/after UI screenshots captured by the `/verify-ui` skill
(`.claude/skills/verify-ui/skill.md`) and embedded in PR descriptions.

Convention: `docs/pr-screenshots/<branch>/<slug>-{before,after}.png`, cropped
to the changed region (`ui-shot.mjs --selector`), ideally under ~300 KB each.
PR bodies reference them via commit-SHA-pinned `raw.githubusercontent.com`
URLs so the images keep rendering after the branch is deleted post-merge.

These files are historical evidence — don't edit them; feel free to prune old
directories if the repo gets heavy.
