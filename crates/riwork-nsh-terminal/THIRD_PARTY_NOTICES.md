# Native terminal source notices

Ghostty is copyright (c) 2024 Mitchell Hashimoto and Ghostty contributors and is
licensed under the MIT license. The complete notice is preserved in
[licenses/Ghostty-MIT.txt](licenses/Ghostty-MIT.txt).

The pinned source closure and its existing modifications are declared in
[VENDOR.md](VENDOR.md). The `gpui-libghostty` distribution is used solely as a
hashed source carrier for that closure. No GPUI Rust code, Cargo dependency,
build script or shim is linked into RiWork's NSH app.

The source closure includes Nerd Fonts symbols and their combined upstream
notices in [licenses/Nerd-Fonts.txt](licenses/Nerd-Fonts.txt). Ghostty's build
also fetches content-hashed native dependencies and fonts, as specified by the
pinned `build.zig.zon` files. Their license files remain in the private source
and Zig package cache; binary distribution packaging must preserve the notices
for the dependencies included in its selected build. This change prepares a
private development installation and does not publish or install a distribution.

The bundled Ghostty themes come from iTerm2-Color-Schemes at the pinned
`release-20260810-152212-0173c3c` release. Its MIT notice, read from that pinned
upstream revision, is preserved in
[licenses/iTerm2-Color-Schemes.txt](licenses/iTerm2-Color-Schemes.txt).
