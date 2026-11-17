# Third-party licenses

Spoon is MIT licensed (see `LICENSE`). It includes or builds on the following.

## SourceGit

- **Source**: https://github.com/sourcegit-scm/sourcegit
- **License**: MIT
- **Used for**: the commit graph lane layout in `src/graph.rs` is a port of SourceGit's
  algorithm; the feature set and git command choices follow SourceGit.

## JetBrains Mono

- **Source**: https://github.com/JetBrains/JetBrainsMono
- **License**: SIL Open Font License, Version 1.1
- **License link**: https://github.com/JetBrains/JetBrainsMono/blob/v2.304/OFL.txt
- **File**: `assets/fonts/JetBrainsMonoNL-Regular.ttf`

## Material Symbols

- **Source**: https://github.com/google/material-design-icons (bundled by the `egui_material_icons` crate)
- **License**: Apache License 2.0

## Rust crates

Direct dependencies, each under its own license (see the crate):

- `eframe` / `egui`: MIT or Apache-2.0
- `egui_material_icons`: MIT
- `material-colors`: MIT or Apache-2.0
- `rfd`: MIT
- `serde`, `serde_json`: MIT or Apache-2.0
- `winresource` (Windows build only): MIT
