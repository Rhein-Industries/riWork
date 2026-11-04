# SOMTUS app icon

Somtus uses the orange and black mark supplied in the root `somtus-icon-export` directory. `somtus-icon.svg` and `somtus-icon-1024.png` here are exact copies of that export. The logo and wordmark are written **SOMTUS**; prose uses **Somtus** and the command line uses `somtus`.

`Somtus.icns` contains the supplied artwork at all standard macOS icon sizes. `scripts/bundle-macos.sh` copies it to `Somtus.app/Contents/Resources/Somtus.icns` and declares it in `CFBundleIconFile`. The updater stages this same file. The old hammer files remain only as historical assets.

The shared UI draws the same SVG geometry in `apps/riwork/Brand.tsx`, alongside the SOMTUS wordmark in Settings. iOS uses the supplied opaque 1024-pixel PNG and applies its system mask.

To regenerate the macOS icon from the supplied PNG:

```sh
iconset="$(mktemp -d)/Somtus.iconset"
mkdir "$iconset"
for spec in 16:icon_16x16 32:icon_16x16@2x 32:icon_32x32 64:icon_32x32@2x \
    128:icon_128x128 256:icon_128x128@2x 256:icon_256x256 512:icon_256x256@2x \
    512:icon_512x512 1024:icon_512x512@2x; do
    size="${spec%%:*}"
    sips -z "$size" "$size" assets/app-icon/somtus-icon-1024.png \
        --out "$iconset/${spec#*:}.png"
done
iconutil -c icns "$iconset" -o assets/app-icon/Somtus.icns
```
