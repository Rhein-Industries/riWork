# Third-party notices for this throwaway spike

The native initialization and API usage were informed by the MIT
`gpui-libghostty` shim. The fixture links an existing local Ghostty native
archive. These notices cover those upstream projects; they are not a complete
distribution audit of every dependency bundled into the native archive.

## gpui-libghostty

Source revision: `2bbde4ae53a8e163cdbe05294bd09934b30c1af3` (crate 0.3.1).
[Pinned upstream license](https://github.com/behzade/gpui-libghostty/blob/2bbde4ae53a8e163cdbe05294bd09934b30c1af3/LICENSE).

```text
MIT License

Copyright (c) 2026 Behzad Ehsani

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Ghostty

License copied from `gpui-libghostty-0.3.1/vendor/ghostty/LICENSE`.
The vendored build identifies itself as `1.3.2-dev`.
[Upstream license](https://github.com/ghostty-org/ghostty/blob/919dca5c848d43a489a4cb2e1feade016f0fa2db/LICENSE).

```text
MIT License

Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

Ghostling was evaluated as an alternative but its source is not included in
this spike. No GPUI crate or gpui-libghostty Entity is used by the fixture.
