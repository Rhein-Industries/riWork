# NSH 0.3 toolchain baseline

This records the isolated NSH baseline for task
`60f8aec4-464b-43fa-bfa1-3980195461dd`, verified on 2026-10-27. It does
not change RiWork's dependencies or its workspace lockfile.

## Verification environment

`verification_environment = isolated /tmp dirs only`

- Scratch root: `/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV`
  (`/private/tmp/...` is the same location on macOS).
- CLI prefix: `$NSH_PROBE/nsh-cli-0.3.0`; invoked by absolute path.
- Mixed app: `$NSH_PROBE/mixed-probe`, with its own `[workspace]`.
- Cargo home: `$NSH_PROBE/cargo-home`; build output: `$NSH_PROBE/target`.
- NSH cache: `$NSH_PROBE/nsh-cache`; temporary files: `$NSH_PROBE/tmp`.
- Host: Apple silicon, `aarch64-apple-darwin`, macOS 26.2 (25C56).
- Xcode: `/Applications/Xcode.app/Contents/Developer`; macOS SDK 26.0.
- No application windows, RiWork runtime, orchestrator, or live task state were
  used for verification. No app was launched, installed, packaged, merged, or
  pushed. No `nsh-platform-gpui` dependency was resolved and `--target gpui`
  was never invoked. The distribution's bundled hosts were not run.

## Pinned versions

| Component | Pin / observed version |
| --- | --- |
| NSH release | `NSH_VERSION=v0.3.0` |
| Isolated CLI | `nsh 0.3.0 (76fa8a7)` |
| Rust toolchain | `1.99.1`, matching RiWork's `rust-toolchain.toml` |
| rustc | `1.99.1 (58cf8f9c3 2026-10-13)`; LLVM `23.1.1` |
| Cargo | `1.99.1 (5f94df478 2026-08-27)` |
| `nsh-core` | crates.io `=0.3.0` |
| `nsh-base` | crates.io `=0.3.0` |
| `nsh-engine` | crates.io `=0.3.0` |
| `nsh-platform-macos` | crates.io `=0.3.0` |

The pre-existing user CLI was `nsh 0.2.0 (68531fb)` at
`~/.nsh/bin/nsh`, selected by the symlink `~/.local/bin/nsh`. Its preservation
check is recorded below.

The [official installer](https://nsh.dev/install.sh) accepts `NSH_INSTALL_DIR`
but still replaces `~/.local/bin/nsh`. Therefore the probe downloaded the
versioned release archive and checksum directly, extracted the complete
distribution into the isolated prefix, and left the user's PATH and symlink
alone. It did not execute the installer or build from the moving source branch.

## Commands

The following reproduces the verified setup. The initial `mktemp` returned the
scratch root recorded above; a new run will get a different suffix. Rust 1.99.1
and Xcode were already present. All commands after creating the scratch root run
there or in its generated app.

```sh
set -eu
export NSH_PROBE="$(mktemp -d /tmp/riwork-nsh-toolchain-60f8aec4.XXXXXX)"
cd "$NSH_PROBE"
export NSH_VERSION=v0.3.0
export NSH_INSTALL_DIR="$NSH_PROBE/nsh-cli-0.3.0"
export NSH_CACHE="$NSH_PROBE/nsh-cache"
export CARGO_HOME="$NSH_PROBE/cargo-home"
export CARGO_TARGET_DIR="$NSH_PROBE/target"
export RUSTUP_TOOLCHAIN=1.99.1
export TMPDIR="$NSH_PROBE/tmp"
mkdir -p "$TMPDIR" "$NSH_INSTALL_DIR"

curl -fL "https://github.com/longbridge/nsh/releases/download/$NSH_VERSION/nsh-macos-arm64.tar.gz" \
  -o nsh-macos-arm64.tar.gz
curl -fsSL "https://github.com/longbridge/nsh/releases/download/$NSH_VERSION/nsh-macos-arm64.tar.gz.sha256" \
  -o nsh-macos-arm64.tar.gz.sha256
shasum -a 256 -c nsh-macos-arm64.tar.gz.sha256
tar -xzf nsh-macos-arm64.tar.gz -C "$NSH_INSTALL_DIR" --strip-components=1
"$NSH_INSTALL_DIR/bin/nsh" --version
"$NSH_INSTALL_DIR/bin/nsh" init "$NSH_PROBE/mixed-probe" --template mixed
cp mixed-probe/Cargo.toml mixed-probe.Cargo.toml.generated

python3 - <<'PY'
from pathlib import Path
p = Path('mixed-probe/Cargo.toml')
s = p.read_text()
assert 'nsh-core = "0.3.0"' in s and 'nsh-base = "0.3.0"' in s
s = s.replace('nsh-core = "0.3.0"', 'nsh-core = "=0.3.0"')
s = s.replace('nsh-base = "0.3.0"', 'nsh-base = "=0.3.0"')
s += '\nnsh-engine = "=0.3.0"\nnsh-platform-macos = "=0.3.0"\n'
p.write_text(s)
Path('mixed-probe/rust-toolchain.toml').write_text(
    '[toolchain]\nchannel = "1.99.1"\n'
)
PY

cd mixed-probe
cargo -V
rustc -vV
cargo check --jobs 4 > "$NSH_PROBE/cargo-check.log" 2>&1
cargo check --locked --offline --jobs 4 > "$NSH_PROBE/cargo-check-locked-offline.log" 2>&1
cargo metadata --locked --offline --format-version 1 \
  --filter-platform aarch64-apple-darwin > "$NSH_PROBE/cargo-metadata.json"
```

The CLI's generated manifest uses caret-compatible `"0.3.0"` requirements for
core/base and does not depend directly on engine/macOS. The scratch-only edits
above make all four requirements exact. The generated `src/lib.rs` is unchanged;
the probe uses default dependency features, including the engine's JS support
and the actual macOS backend on this host.

Release archive SHA-256:

```text
34fdd2ec43d5e22986723e7633acf9223e365bd5bace1a502b5ae24fb75c266c  nsh-macos-arm64.tar.gz
```

Extracted CLI SHA-256:

```text
90b7633c80306407fc97b2051be00af868d4577281f918ff655c650a8a5a2e60  bin/nsh
```

## Results and lockfile notes

All setup and verification commands above exited 0. Observed output:

```text
nsh-macos-arm64.tar.gz: OK
nsh 0.3.0 (76fa8a7)
✓ created mixed-probe: .gitignore, App.tsx, App.test.tsx, tsconfig.json, Cargo.toml, src/lib.rs, README.md, nsh.json

# Initial cargo check (selected final lines):
    Checking nsh-core v0.3.0
    Checking nsh-base v0.3.0
    Checking nsh-engine v0.3.0
    Checking nsh-platform-macos v0.3.0
    Checking nsh-app-mixed-probe v0.1.0 (/private/tmp/riwork-nsh-toolchain-60f8aec4.18l3zV/mixed-probe)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 24s

# cargo check --locked --offline --jobs 4:
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.05s
```

This establishes Rust compilation/type checking with default features on the
macOS host. Final executable linkage, TSX bundling, and UI execution were not
part of this check.

The initial check generated the probe's `Cargo.lock` (format 4, 391 packages).
Its SHA-256 after both checks:

```text
2e1b00b9b29f62c8d47ba5bc50f8ac72d97edb0957f20fc5e418ec0fe4133a02
```

The following inspection ran from `mixed-probe`, exited 0, and confirmed one
copy of each requested crate at `0.3.0` in both the lockfile and Cargo metadata.
Every external dependency is sourced from
`registry+https://github.com/rust-lang/crates.io-index`; there are no Git/path
overrides or GPUI packages. The generated local probe itself has no registry
source.

```sh
python3 - <<'PY' > "$NSH_PROBE/lock-summary.txt"
from pathlib import Path
import hashlib, json, tomllib
lock_path=Path('Cargo.lock')
lock=tomllib.loads(lock_path.read_text())
registry='registry+https://github.com/rust-lang/crates.io-index'
names=('nsh-core','nsh-base','nsh-engine','nsh-platform-macos')
metadata=json.loads(Path('../cargo-metadata.json').read_text())
for name in names:
    entries=[p for p in lock['package'] if p['name']==name]
    assert len(entries)==1 and entries[0]['version']=='0.3.0' and entries[0]['source']==registry
    resolved=[p for p in metadata['packages'] if p['name']==name]
    assert len(resolved)==1 and resolved[0]['version']=='0.3.0' and resolved[0]['source']==registry
    print(name, entries[0]['version'], entries[0]['source'], entries[0]['checksum'])
assert all(p.get('source')==registry for p in lock['package'] if p['name']!='nsh-app-mixed-probe')
assert not any('gpui' in p['name'] for p in lock['package'])
print('Lock format:',lock['version'])
print('Package count:',len(lock['package']))
print('Lock SHA-256:',hashlib.sha256(lock_path.read_bytes()).hexdigest())
print('PASS: lock and Cargo metadata agree; all external crates registry-sourced; no GPUI packages')
PY
```

The lockfile also resolves `nsh-js`, `nsh-macros`, and `nsh-package` to `0.3.0`.
Only the four requested direct dependencies have exact manifest pins. Fresh
resolution can select newer transitive dependencies; preserving this scratch
`Cargo.lock` and using `--locked` is necessary to replay the same graph. The
offline replay above used the isolated cache populated by the initial check.

Raw evidence remains at the scratch root: `cargo-check.log`,
`cargo-check-locked-offline.log`, `cargo-metadata.json`, `lock-summary.txt`, and
`mixed-probe/Cargo.lock`. The original generated manifest is saved as
`mixed-probe.Cargo.toml.generated`. `probe-env.sh` restores this run's environment.

## User-install preservation

Before extraction, all 49 files under `~/.nsh` were SHA-256 hashed into
`$NSH_PROBE/user-nsh-before.json` along with the `~/.local/bin/nsh` symlink
destination. The following comparison ran from the scratch root after setup;
it exited 0:

```sh
python3 - <<'PY'
from pathlib import Path
import hashlib, json
root=Path('/Users/dominik/.nsh')
before=json.loads(Path('user-nsh-before.json').read_text())
after={str(p.relative_to(root)):hashlib.sha256(p.read_bytes()).hexdigest()
       for p in sorted(root.rglob('*')) if p.is_file()}
link=str(Path('/Users/dominik/.local/bin/nsh').readlink())
assert before=={'files':after,'link':link}
print(f'PASS: all {len(after)} user NSH file hashes and ~/.local/bin/nsh symlink unchanged')
PY
```

Result: `PASS: all 49 user NSH file hashes and ~/.local/bin/nsh symlink unchanged`.
The existing CLI still reports `nsh 0.2.0 (68531fb)`. RiWork's `Cargo.toml`,
`Cargo.lock`, and `rust-toolchain.toml` remain unchanged. Task status is left to
the orchestrator.

Only this document belongs in the RiWork commit. The CLI, generated app, Cargo
lockfile, cache, and build output stay in `/tmp`; no `.gitignore` change is
needed. The isolated prefix is temporary: repeat the commands if it is removed.

## References

- [NSH v0.3.0 release](https://github.com/longbridge/nsh/releases/tag/v0.3.0)
- [Installation guidance](https://nsh.dev/docs/installation/)
- [v0.3.0 mixed-template generator](https://github.com/longbridge/nsh/blob/v0.3.0/crates/nsh-cli/src/init.rs)
- [v0.3.0 macOS crate manifest](https://github.com/longbridge/nsh/blob/v0.3.0/crates/nsh-platform-macos/Cargo.toml)
