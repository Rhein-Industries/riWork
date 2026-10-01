# RiWork app icon

The flat monochrome block hammer is RiWork's macOS app icon. The artwork is drawn directly as SVG paths, with no text or embedded bitmap.

- `riwork-hammer-legacy.svg` and `RiWork-legacy.icns`: the rounded icon RiWork ships on every supported macOS version. The filenames retain their original "legacy" name. This is the only icon the bundle script reads.
- `riwork-hammer.svg`: editable 1024 × 1024 full-bleed artwork, a square `#222222` tile with the hammer on it. It is the source for `RiWork.icns`.
- `RiWork.icns`: compiled full-bleed icon. It is not bundled; macOS supplies the rounded mask for full-bleed icons, so it is kept for Icon Composer-style workflows.
- `hammer-mark.svg`: transparent foreground artwork, the layer to import into Icon Composer.

The iOS app uses the same full-bleed artwork as `ios/RiWorkRemote/Assets.xcassets/AppIcon.appiconset/AppIcon.png`: the 1024-pixel render of `riwork-hammer.svg`, flattened onto `#222222` with no alpha channel, because iOS rejects app icons with transparency and applies its own mask.

The bundle script copies `RiWork-legacy.icns` to `Contents/Resources/RiWork.icns` and declares it with `CFBundleIconFile`. Local updates stage only that rounded icon before packaging, so a checkout without `RiWork.icns` still builds and updates.

## Regenerating the `.icns` files

Render the SVG at the pixel sizes below into a directory named `<name>.iconset`, then run `iconutil -c icns` on it. This shell function does both with `rsvg-convert` (`brew install librsvg`); any SVG renderer that keeps transparency works in its place. Run it from this directory.

```sh
render_icns() { # render_icns SOURCE.svg OUTPUT.icns
    iconset="$(mktemp -d)/$(basename "$2" .icns).iconset"
    mkdir "$iconset"
    for spec in 16:icon_16x16 32:icon_16x16@2x 32:icon_32x32 64:icon_32x32@2x \
        128:icon_128x128 256:icon_128x128@2x 256:icon_256x256 512:icon_256x256@2x \
        512:icon_512x512 1024:icon_512x512@2x; do
        rsvg-convert --width "${spec%%:*}" --height "${spec%%:*}" "$1" \
            --output "$iconset/${spec#*:}.png"
    done
    iconutil -c icns -o "$2" "$iconset"
}

render_icns riwork-hammer-legacy.svg RiWork-legacy.icns   # the shipped rounded icon
render_icns riwork-hammer.svg RiWork.icns                 # the full-bleed export
```

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

The rounded source keeps its transparent margin around the tile, so its PNGs must keep their alpha channel.

## Icon Composer

Import `hammer-mark.svg` and set a solid charcoal background (`#222222`) in Composer. Its system mask supplies the rounded tile; a full-bleed export such as `RiWork.icns` avoids a second, smaller tile inside the system frame. Do not import `riwork-hammer-legacy.svg` there, because it already draws its own rounded tile.
