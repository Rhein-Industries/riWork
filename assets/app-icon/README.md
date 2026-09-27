# RiWork app icon

The flat monochrome block hammer is RiWork's macOS app icon. The artwork is drawn directly as SVG paths, with no text or embedded bitmap.

- `riwork-hammer.svg`: editable 1024 × 1024 app icon with an opaque full-bleed charcoal background for macOS 26 and later.
- `hammer-mark.svg`: transparent foreground artwork for Icon Composer.
- `RiWork.icns`: compiled full-bleed macOS icon with 16–1024 px representations.
- `riwork-hammer-legacy.svg` and `RiWork-legacy.icns`: rounded fallback for macOS versions before 26.

The bundle script selects the full-bleed icon on macOS 26 and later, and the rounded fallback on earlier versions. It copies the selected file to `Contents/Resources/RiWork.icns` and declares it with `CFBundleIconFile`. Local updates stage both icon resources before packaging.

To regenerate the `.icns`, render `riwork-hammer.svg` at the pixel sizes below into a directory named `RiWork.iconset`, then run `iconutil -c icns -o RiWork.icns RiWork.iconset`. An SVG renderer such as `rsvg-convert` can render each PNG directly from the source.

| Filename | Pixels |
| --- | --- |
| `icon_16x16.png` | 16 |
| `icon_16x16@2x.png` | 32 |
| `icon_32x32.png` | 32 |
| `icon_32x32@2x.png` | 64 |
| `icon_128x128.png` | 128 |
| `icon_128x128@2x.png` | 256 |
| `icon_256x256.png` | 256 |
| `icon_256x256@2x.png` | 512 |
| `icon_512x512.png` | 512 |
| `icon_512x512@2x.png` | 1024 |

For Icon Composer, import `hammer-mark.svg` and set a solid charcoal background (`#222222`) in Composer. Its system mask supplies the rounded tile; the full-bleed export avoids a second, smaller tile inside the system frame.
