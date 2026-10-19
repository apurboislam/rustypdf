# Bundled fonts

Both fonts are licensed under the SIL Open Font License 1.1 (see `OFL.txt` in
each folder). They are baked into the Docker image and loaded once at startup.
`REQUIRED_FONTS` (default `Noto Sans Bengali,Inter`) must be present or the
service refuses to start.

| Folder | Family | Version | Files | Source |
|---|---|---|---|---|
| `inter/` | Inter | 4.001 (release v4.1) | 18 static TTFs (Thin–Black, upright + italic) | <https://github.com/rsms/inter/releases/tag/v4.1> (`Inter-4.1.zip`, `extras/ttf/Inter-*.ttf`) |
| `noto-sans-bengali/` | Noto Sans Bengali | 3.011 (hinted) | 9 static TTFs (Thin–Black) | <https://github.com/notofonts/notofonts.github.io/tree/main/fonts/NotoSansBengali/hinted/ttf> |

Static (non-variable) files are used so every weight is a distinct face.
Any extra `.ttf`/`.otf`/`.ttc` mounted under `FONTS_DIR` (for example a brand
font in `/fonts/extra`) is picked up too.

Upgrading a font changes rendered output: re-run the golden tests and approve
the new images on purpose (`just golden-update`).
