# Offline Mermaid worker

`mermaid-11.17.2.min.js` is the unmodified full browser bundle from the official
`mermaid@11.17.2` npm package. Its transitive license notices are retained at the
end of the bundle and in `THIRD_PARTY_NOTICES.txt`; Mermaid's MIT license is adjacent.
Both notices are packaged in the app's `Contents/Resources/licenses` directory.
There is no runtime npm,
CDN, external browser, or rendering service dependency.

Source: https://registry.npmjs.org/mermaid/-/mermaid-11.17.2.tgz
Package integrity: `sha512-V6K3C8EBdEsPFZXSKMJe6ppQOENxuHARr9GvHX4hh47lAbhMRD9qf4oEK7LoaRQxULMa80/qt5gHO73aCleBBg==`
Bundle SHA-256: `581ed7d74bd9048d0e3a91363927d72ef22942d7722546b27f7cc29e35390eb8`

`build.rs` compiles `main.swift` into `riwork-diagram`; the macOS bundle copies
and signs it beside `riwork`. Rust embeds the JavaScript assets and sends them
with each bounded render request over stdin. The worker owns a nonpersistent,
windowless WKWebView, denies navigation and network/subresource access, locks
Mermaid security settings, and returns a raster PNG. Rust owns cancellation,
process cleanup, concurrency, cache, and response limits. Syntax errors and
resource limits leave the original source available in the transcript.
