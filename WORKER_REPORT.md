# Worker report: iOS export encryption declaration

Task `02201690-3e7f-4a79-bdad-c819fa4a6662` on project `39832c2e-23a5-476d-aa8f-5ff34a02d314`, branch `chore/ios-encryption-declaration`.

`ITSAppUsesNonExemptEncryption` stays absent from `ios/project.yml`, `ios/RiWorkRemote/Info.plist`, and `ios/RiWorkRemote/Info-Debug.plist`. Apple’s pages define that Boolean as an exemption claim (`NO`) or a non-exempt claim (`YES`, normally with an Apple-issued `ITSEncryptionExportComplianceCode`). Neither value is justified yet. This file is an engineering record. It is not a legal attestation, a BIS filing, an App Store submission, or an Apple approval.

Account-holder inputs are in `ios/docs/export-compliance.md`.

## Citations

Retrieved 2026-09-29.

[Overview of export compliance](https://developer.apple.com/help/app-store-connect/manage-app-information/overview-of-export-compliance)

> If your app uses, accesses, contains, implements, or incorporates encryption, and you intend to upload, test, and distribute it, you need to determine your export compliance requirements in App Store Connect.

Examples that require a determination include standard encryption algorithms, crypto functionality within Apple’s operating system, and proprietary or non-standard encryption algorithms. The page quotes the US Government definition of non-standard cryptography as proprietary or unpublished cryptographic functionality, including algorithms or protocols that a recognized standards body has not adopted and that have not otherwise been published. It says the reader must review the Export Administration Regulations to decide whether a CCATS is required, and that an inaccurate exemption claim is the submitter’s liability.

Its next-step table: when no documentation is required, update Info.plist so the questions are not repeated; when documentation is required, submit it, attach the approved documentation to the build, then update Info.plist.

[Complying with Encryption Export Regulations](https://developer.apple.com/documentation/Security/complying-with-encryption-export-regulations)

> Set the value to `NO` if your app—including any third-party libraries it links against—doesn’t use encryption, or if it only uses forms of encryption that are exempt from export compliance documentation requirements. Otherwise, set it to `YES`.

> Typically, the use of encryption that’s built into the operating system—for example, when your app makes HTTPS connections using URLSession—is exempt from export documentation upload requirements, whereas the use of proprietary encryption is not.

The same page says exempt encryption might still require a year-end self-classification report to the US government, and points to [BIS annual self-classification](https://www.bis.doc.gov/index.php/policy-guidance/encryption/4-reports-and-reviews/a-annual-self-classification). The overview links [BIS encryption policy](https://www.bis.doc.gov/index.php/policy-guidance/encryption). This change does not file that report.

[ITSAppUsesNonExemptEncryption](https://developer.apple.com/documentation/bundleresources/information-property-list/itsappusesnonexemptencryption)

`NO` means no encryption, or only encryption exempt from export compliance requirements. `YES` means non-exempt encryption. With `YES`, the page says you typically also set `ITSEncryptionExportComplianceCode` to a code Apple provides after reviewing documentation. With the key absent, App Store Connect asks the questionnaire on every upload.

[ITSEncryptionExportComplianceCode](https://developer.apple.com/documentation/bundleresources/information-property-list/itsencryptionexportcompliancecode)

Include this key when `ITSAppUsesNonExemptEncryption` is `YES`, using the code Apple sends after review. No such code exists for this app.

[Export compliance documentation for encryption](https://developer.apple.com/help/app-store-connect/reference/app-information/export-compliance-documentation-for-encryption)

| Encryption algorithm in use | Documentation in App Store Connect |
| --- | --- |
| Limited to encryption within the Apple operating system | None |
| Industry standard algorithm not provided within the Apple operating system | French declaration, only if distributed on the App Store in France |
| Proprietary algorithms not accepted by international standard bodies (IEEE, IETF, ITU, and the like) | US CCATS, plus a French declaration if distributed in France |

[Determine and upload app encryption documentation](https://developer.apple.com/help/app-store-connect/manage-app-information/determine-and-upload-app-encryption-documentation) and [Provide export compliance information for beta builds](https://developer.apple.com/help/app-store-connect/test-a-beta-version/provide-export-compliance-information-for-beta-builds) limit that questionnaire to the Account Holder, Admin, or App Manager. Apple says a complete documentation review is case by case and expects about two business days.

## Why the Boolean is not set

The shipped app does use encryption, so “no encryption” is not available. The uses are wider than the HTTPS example: CryptoKit ChaCha20-Poly1305 encrypts application payloads, including terminal text and commands (`ios/Core/SessionCrypto.swift`, contract in `docs/remote-protocol.md`). The algorithms are IETF/FIPS constructions implemented by CryptoKit and Security, and the built binaries do not link a third-party cipher. The handshake protocol itself is a RiWork protocol in the public repository `https://github.com/Rhein-Industries/riWork` (`private: false`), not an IETF protocol. France availability is not chosen in this repo, while the overview says France controls secure communications. No App Store Connect questionnaire result and no Apple-issued code are on file.

Those facts sit on more than one row of Apple’s documentation table. Choosing `NO` would claim an exemption. Choosing `YES` would claim non-exempt encryption and, on Apple’s key page, belongs with a code Apple has not issued. The key stays absent so upload still presents the questionnaire.

## Shipped inventory

App target `RiWorkRemote` embeds first-party `RiWorkCore`. `ios/Package.swift` has no external dependencies. No Podfile, Cartfile, or `Package.resolved`. Test and smoke targets are not in the app product.

| Mechanism | Implementation | Linked from the app product |
| --- | --- | --- |
| HMAC-SHA256 handshake MAC | CryptoKit, 32-byte PSK | `RiWorkCore` → CryptoKit |
| SHA-256 transcript hash | CryptoKit | same |
| HKDF-SHA256, two 32-byte traffic keys | CryptoKit | same |
| ChaCha20-Poly1305 payload seal/open (RFC 8439) | CryptoKit `ChaChaPoly` | same |
| 32-byte client nonce | `SecRandomCopyBytes` | `RiWorkCore` → Security |
| Pairing material at rest | Keychain, this device only, not synchronizable | Security |
| WebSocket | `URLSessionWebSocketTask` | Foundation. Release pairing requires `wss://`. Debug UI can allow loopback `ws://` |

`otool -L` on Debug and Release (arm64 and x86_64): `RiWorkCore` links `/System/Library/Frameworks/CryptoKit.framework/CryptoKit` and `/System/Library/Frameworks/Security.framework/Security`. The app executable links `@rpath/RiWorkCore.framework/RiWorkCore` plus system UI frameworks (Foundation, SwiftUI, UIKit, AVFoundation, Vision, VisionKit). No `libcrypto`, OpenSSL, libsodium, or BoringSSL load command in either binary.

## Checks

Checked-in plists differ only by Debug `NSAppTransportSecurity` / `NSAllowsLocalNetworking` = true. Release has no `NSAppTransportSecurity`. Neither file contains `ITSAppUsesNonExemptEncryption` or `ITSEncryptionExportComplianceCode` (`PlistBuddy`: `Entry, ":ITSAppUsesNonExemptEncryption", Does Not Exist`).

`xcodegen` 2.44.1 `generate` in `ios/` rewrote nothing. `git diff` after generate was only the new comment in `ios/project.yml`. `postGenCommand` still copies `Info.plist` to `Info-Debug.plist` and adds only the Debug ATS exception, so a later Boolean in `info.properties` would land in both plists.

Unsigned simulator builds, Xcode 26.0.1 (17A400), `CODE_SIGNING_ALLOWED=NO`, destination `generic/platform=iOS Simulator`, derived data `/tmp/riwork-export-compliance-dd`:

```sh
xcodebuild -project ios/RiWorkRemote.xcodeproj -scheme RiWorkRemote \
  -configuration Debug -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath /tmp/riwork-export-compliance-dd CODE_SIGNING_ALLOWED=NO build
xcodebuild -project ios/RiWorkRemote.xcodeproj -scheme RiWorkRemote \
  -configuration Release -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath /tmp/riwork-export-compliance-dd CODE_SIGNING_ALLOWED=NO build
```

Both ended `** BUILD SUCCEEDED **`.

| Built Info.plist | `ITSAppUsesNonExemptEncryption` | `NSAppTransportSecurity` |
| --- | --- | --- |
| `Debug-iphonesimulator/RiWorkRemote.app/Info.plist` | absent | `NSAllowsLocalNetworking` true |
| `Release-iphonesimulator/RiWorkRemote.app/Info.plist` | absent | absent |

The ad-hoc Debug app produced by the test command below also has the key absent and `NSAllowsLocalNetworking` true.

```sh
swift test --package-path ios
```

`** 31 tests, 0 failures **` (15 `ProtocolTests` + 16 `RelayClientTests`) at 2026-09-29 02:22:58.

```sh
xcodebuild -project ios/RiWorkRemote.xcodeproj -scheme RiWorkRemote \
  -destination 'platform=iOS Simulator,id=45D942B2-ABE6-4C4F-8D13-E252AF668880' \
  -derivedDataPath /tmp/riwork-export-compliance-test-dd CODE_SIGN_IDENTITY=- test
```

`OS=26.0` does not match this machine; the iPhone 17 Pro runtime is iOS 26.0.1 (`45D942B2-ABE6-4C4F-8D13-E252AF668880`). Retried with that simulator id. `** TEST SUCCEEDED **`. `RiWorkCoreTests` 31 tests, 0 failures. `RiWorkAppTests` 22 tests, 0 failures. Result bundle: `/tmp/riwork-export-compliance-test-dd/Logs/Test/Test-RiWorkRemote-2026.09.29_02-25-11-+0200.xcresult`. No physical device was used.

## Left for the account holder

Answer the questionnaire at App Information → App Encryption Documentation, or on a TestFlight build via Provide Export Compliance Information. The facts to take into that dialog are in `ios/docs/export-compliance.md`. After that role records an outcome, set one Boolean in `info.properties` and regenerate so Debug and Release match. Add `ITSEncryptionExportComplianceCode` only with the code Apple displays. Distribution in France, any French declaration, any CCATS, and any BIS annual report remain that role’s decisions.

```json
{
  "task_id": "02201690-3e7f-4a79-bdad-c819fa4a6662",
  "project_id": "39832c2e-23a5-476d-aa8f-5ff34a02d314",
  "branch": "chore/ios-encryption-declaration",
  "its_app_uses_non_exempt_encryption": null,
  "justified": false,
  "its_encryption_export_compliance_code": null,
  "legal_attestation": false,
  "submitted": false,
  "apple_approval_claimed": false,
  "reason": "The app uses CryptoKit, Security, and URLSession encryption, including ChaCha20-Poly1305 payload confidentiality. Apple defines the Boolean as an exemption or non-exempt claim, and YES normally requires an Apple-issued code. The account-holder questionnaire, France availability, and that code are unresolved, so the key stays absent in project.yml and both plists.",
  "sources_retrieved": "2026-09-29",
  "sources": [
    "https://developer.apple.com/help/app-store-connect/manage-app-information/overview-of-export-compliance",
    "https://developer.apple.com/documentation/Security/complying-with-encryption-export-regulations",
    "https://developer.apple.com/documentation/bundleresources/information-property-list/itsappusesnonexemptencryption",
    "https://developer.apple.com/documentation/bundleresources/information-property-list/itsencryptionexportcompliancecode",
    "https://developer.apple.com/help/app-store-connect/manage-app-information/determine-and-upload-app-encryption-documentation",
    "https://developer.apple.com/help/app-store-connect/reference/app-information/export-compliance-documentation-for-encryption"
  ],
  "files": [
    "ios/project.yml",
    "ios/RiWorkRemote/Info.plist",
    "ios/RiWorkRemote/Info-Debug.plist",
    "ios/docs/export-compliance.md",
    "WORKER_REPORT.md"
  ],
  "plist_key_in_checked_in_and_built_debug_and_release": "absent",
  "checks": {
    "xcodegen": "2.44.1 generate left plists and pbxproj unchanged",
    "debug_release_build": "CODE_SIGNING_ALLOWED=NO generic iOS Simulator, both BUILD SUCCEEDED",
    "swift_test": "31 tests, 0 failures",
    "xcodebuild_test": "iPhone 17 Pro simulator iOS 26.0.1, 31 core + 22 app tests, 0 failures, TEST SUCCEEDED"
  }
}
```
