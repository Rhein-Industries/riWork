# Private-build integrity test record

Command executed earlier in this session from the repository root:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 crates/riwork-nsh-terminal/scripts/test_prepare_ghostty.py
```

Observed result: 9 tests passed, exit 0. This is a worker-recorded result from the tool response; a standalone raw transcript was not retained and is not fabricated here. Tests use temporary files only and do not launch an app, compile a library, download source, or access live state.

Coverage: valid imported provenance; modified archive/header/bundled theme rejected; a rewritten imported-artifact digest cannot bypass the accepted GO hash; unknown provenance rejected; tar traversal and symbolic links rejected; source archive hash mismatch rejected before extraction.

Source SHA256:
- `crates/riwork-nsh-terminal/scripts/test_prepare_ghostty.py`: `5ee18ab89251a2ca9f26e202248183aec071ab2ae02074ed739d5a2398217faf`
- `crates/riwork-nsh-terminal/scripts/prepare_ghostty.py`: `adf48cc091d62ec26f82a1b36a3cd2c38212415b490f5c019df7d9048c806d49`
- `crates/riwork-nsh-terminal/ghostty-pin.json`: `1aca811cadf03fea2c8e31bafebd81259db925bdd802064be0b284532429fa13`
