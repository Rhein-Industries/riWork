# Pinned libghostty build

The native terminal links the **Ghostty C static library** at upstream commit
`9f0e1719dc918368367d368bfe300f59bb68b5a4`, built with **Zig 0.16.0**. It does not
depend on or link the `ghostty`, `gpui`, or `gpui-libghostty` Rust packages.
The first production slice supports native `aarch64-apple-darwin` builds.

`ghostty-pin.json` is the machine-readable pin. `build.rs` verifies preparation
provenance, target, archive hash and C header hash before compiling the Objective-C
adapter. Missing or mismatched artifacts fail the build. It never falls back to
the user's GPUI cache, searches for an arbitrary system library, or downloads
code during Cargo compilation.

## Prepare from source

From the repository root, with an existing Zig 0.16.0 binary and Xcode toolchain:

```sh
python3 crates/riwork-nsh-terminal/scripts/prepare_ghostty.py build \
  --zig "$HOME/.local/share/riwork/toolchains/zig-0.16.0/zig"
cargo build -p riwork-nsh-terminal
```

The Zig executable's version and SHA-256 must match the arm64 macOS pin. The
script neither installs Zig nor modifies the existing toolchain. It uses the
selected Xcode toolchain (`DEVELOPER_DIR` is honored), with `SDKROOT`, `CC`, `AR`
and `LD` resolved through `xcrun`.

Every source, download, dependency, compiler-cache, build-log and installation
write is beneath `crates/riwork-nsh-terminal/.runtime/pinned`, which is ignored
by Git. Both Zig cache locations are explicitly set inside that directory.
The complete verified source remains in `source`; compilation uses a private
`build-source` copy because Zig 0.16 creates its `zig-pkg` dependency directory
beside `build.zig`. The verified source is checked again after compilation.
There is no system-wide installation, shared Zig-cache write or Cargo registry
source mutation. `--cache-dir /absolute/private/path` can select another private
directory; pass its `install` directory to Cargo with
`RIWORK_GHOSTTY_INSTALL_DIR=/absolute/private/path/install`.

The build emits `install/lib/libghostty-internal.a`, the matching header, and
`install/provenance.json`. Provenance records the source pin, source hashes,
Zig binary hash, command, selected SDK/tool paths, cache paths, build-log hash
and resulting artifact hashes. It also prepares the pinned theme resources below.
The fixed options are in `ghostty-pin.json`:
`ReleaseFast`, `app-runtime=none`, and no macOS app, XCFramework, docs, terminfo,
benchmarks, web data, i18n or Sentry. Parallelism defaults to four jobs.

Rebuilds on another SDK/path may have a different archive SHA-256. Their actual
hash is recorded and enforced on subsequent Cargo builds; that does **not**
give them the pinned GO fixture's runtime acceptance result. This assignment
uses the exact GO archive below for production verification.

## Source distribution and declared patches

The hashed source carrier is the published `gpui-libghostty-0.3.1.crate` tarball:

```text
https://static.crates.io/crates/gpui-libghostty/gpui-libghostty-0.3.1.crate
SHA-256 2b12d0d271cf0ca99a9073bdc148ce43782713c6d01be33af2a105b00c62ef5d
```

Only `gpui-libghostty-0.3.1/vendor/ghostty/` is extracted. Its Rust package,
adapter, Cargo manifest, build script and dependencies are never extracted into
the source tree, built, or linked. Using this source carrier preserves the exact
patched source closure that passed the entry gate, without adding a Rust crate
dependency. The carrier's VCS revision is
`2bbde4ae53a8e163cdbe05294bd09934b30c1af3`.

All **1,242** extracted source files must match the gate's complete source
manifest SHA-256:
`78f97c2dae6f6717963a8de12a4164c6d20c57bd7f984536878d2b98bc9816de`.
The algorithm hashes files sorted by relative path, feeding each UTF-8 path and
its bytes with a little-endian unsigned 64-bit length before each. Archive
symlinks, special files and path traversal are rejected. The pinned source's
`build.zig.zon` files carry Zig's content hashes for fetched dependencies, and
Zig verifies those when populating the private package cache.

The preserved [VENDOR.md](VENDOR.md) declares the prior source modifications:
source-closure pruning, installing the internal static archive, caller-owned
OpenGL context API, GLAD linkage and `SDKROOT` support. Its SHA-256 is
`028e01e535156bfed0c7b861d2b7017c4b4aa8db5dc733286c7629e00e66ca2c`.
These are the already declared gate patches. No additional Ghostty source patch
is applied by this production crate.

Fetch and verify without building:

```sh
python3 crates/riwork-nsh-terminal/scripts/prepare_ghostty.py fetch
```

For offline source preparation, add `--source-archive /path/to/the/exact.crate`
to `fetch` or `build`. The archive hash and extracted source manifest are still
mandatory. This copies from the given path; it does not mutate it. A complete
offline rebuild additionally needs Zig's pinned dependency packages in the
selected private cache.

## Reuse the exact accepted archive

An existing private GO installation can be copied with:

```sh
python3 crates/riwork-nsh-terminal/scripts/prepare_ghostty.py import-gate \
  --from-install /absolute/path/to/pinned/install
python3 crates/riwork-nsh-terminal/scripts/prepare_ghostty.py verify
```

Import only succeeds for the following fixed pair and copies both to the private
cache, verifying each copy. It never uses the historically cached NO-GO archive.

| Build input | SHA-256 |
| --- | --- |
| `lib/libghostty-internal.a` | `b51367aa565527eddfa180d5ae6304899d903af6fc094b4531671ffc20fb064f` |
| `include/ghostty.h` | `f2c12e1bb02540d79feed6bd29ee37312c4e0d24391442a63e66f06e1f7f2e4a` |

Both `build` and `import-gate` refuse to overwrite an installation. To compare
a fresh rebuild with the gate archive, select a separate `--cache-dir`.
The provenance is an integrity/build record, not a signed supply-chain
attestation.

## Bundled theme resources

`build` and `import-gate` also populate `install/share/ghostty/themes` with the
**602 themes** from the exact `iterm2_themes` dependency pinned in Ghostty's
`build.zig.zon`. The separately hashed tarball is
`ghostty-themes-release-20260810-152212-0173c3c.tgz` from
`https://deps.files.ghostty.org/`, SHA-256
`ea9878471420ee5b12e7f2ff480099c954ea50e573a1bdf83f43e105c9be63f0`.
Its extracted manifest is
`72de45b75c66d97dac20bc58a843b6a2fbb4fb39f99261b977e28aeea30c29fe`,
independently matched to Zig's verified package
`N-V-__8AAPpcBAD4_75xLBbLiYqdojOwQP74eoWmpL3jPrBl`.

Every theme is hash-checked before each native build. `build.rs` passes the
resource path as the C string macro and Rust compile-time environment value
`RIWORK_GHOSTTY_RESOURCES_DIR`. The adapter can resolve built-in theme names to
absolute files without changing process environment after worker threads start.
User theme files keep their normal precedence. This is a development path;
future distributable app packaging must copy resources into its app bundle and
resolve that bundle location.

The resource fetch uses macOS's `curl` because the dependency CDN rejects
Python's default HTTP user agent. For an already prepared archive, the explicit
`resources` action stages and verifies only these missing resources. It refuses
to overwrite a theme directory whose manifest differs.

The original GO artifact and toolchain provenance is checked in at
`spikes/ghostty-nsh/evidence/phase2/pinned-runtime/artifact-provenance.json`.
The gate is documented in `spikes/ghostty-nsh/WORKER_REPORT.md`.

## Link verification

```sh
cargo tree -p riwork-desktop --prefix none > /tmp/riwork-nsh-cargo-tree.txt
rg '^(gpui|gpui-libghostty|ghostty)( | v)' /tmp/riwork-nsh-cargo-tree.txt
```

The search must have no matches (exit 1). The adapter links only
`static=ghostty-internal`, `c++`, `objc`, and the required macOS frameworks.
`riwork-nsh-terminal` is RiWork's own adapter crate, not a Ghostty Rust binding
dependency.

Build-integrity regression checks (changed archives/headers, forged import
hashes, unknown preparation provenance and unsafe source tarballs):

```sh
PYTHONDONTWRITEBYTECODE=1 python3 crates/riwork-nsh-terminal/scripts/test_prepare_ghostty.py
```
