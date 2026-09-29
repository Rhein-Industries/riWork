# Worker report: iOS export encryption declaration

Task `02201690-3e7f-4a79-bdad-c819fa4a6662` on project `39832c2e-23a5-476d-aa8f-5ff34a02d314`, branch `chore/ios-encryption-declaration`.

## Correction

`ITSAppUsesNonExemptEncryption` is `false` in `ios/project.yml`, `ios/RiWorkRemote/Info.plist`, and `ios/RiWorkRemote/Info-Debug.plist`. `ITSEncryptionExportComplianceCode` is absent.

The previous report said the published RiWork message format spanned several rows of Apple’s documentation table. The cited pages do not say that a custom application protocol is non-exempt when the encryption is limited to the Apple operating system. That claim is withdrawn. This file is still not a legal attestation, a BIS filing, a submission, or an Apple approval.

## Official text

Retrieved again 2026-09-29.

[Export compliance documentation for encryption](https://developer.apple.com/help/app-store-connect/reference/app-information/export-compliance-documentation-for-encryption), first row:

> Your app uses encryption limited to that within the Apple operating system
>
> No documentation required in App Store Connect.

The next row is “an industry standard algorithm, not provided within the Apple operating system,” which asks for a French declaration only if the app is distributed in France. The last row is “proprietary encryption algorithms not accepted by international standard bodies (such as IEEE, IETF, or ITU),” which asks for a US CCATS and, if distributed in France, a French declaration.

[Complying with Encryption Export Regulations](https://developer.apple.com/documentation/Security/complying-with-encryption-export-regulations):

> Set the value to `NO` if your app—including any third-party libraries it links against—doesn’t use encryption, or if it only uses forms of encryption that are exempt from export compliance documentation requirements. Otherwise, set it to `YES`.

> Typically, the use of encryption that’s built into the operating system—for example, when your app makes HTTPS connections using URLSession—is exempt from export documentation upload requirements, whereas the use of proprietary encryption is not.

[ITSAppUsesNonExemptEncryption](https://developer.apple.com/documentation/bundleresources/information-property-list/itsappusesnonexemptencryption) uses the same `NO` / `YES` split. `YES` is the value that page says is normally paired with `ITSEncryptionExportComplianceCode` after Apple reviews documentation. The overview’s next step when no documentation is required is to update Info.plist so the questions are not repeated.

The overview defines non-standard cryptography as:

> any implementation of “cryptography” involving the incorporation or use of proprietary or unpublished cryptographic functionality, including encryption algorithms or protocols that have not been adopted or approved by a duly recognized international standards body (e.g., IEEE, IETF, ISO, ITU, ETSI, 3GPP, TIA, and GSMA) and haven’t otherwise been published.

That sentence does not say an application format which only calls operating-system implementations of standard algorithms requires documentation. No quoted page contains that rule, so the first row stands.

## Binary the row is applied to

Prior `otool -L` on Debug and Release, arm64 and x86_64, still describes this tree: `RiWorkCore` links CryptoKit and Security only among crypto frameworks. The app executable links `RiWorkCore` and system frameworks, including Foundation. No `libcrypto`, OpenSSL, libsodium, or BoringSSL. The calls are CryptoKit HMAC-SHA256, SHA-256, HKDF-SHA256, and ChaCha20-Poly1305, Security’s random bytes and Keychain, and `URLSession` WebSockets. `ios/Package.swift` has no third-party packages.

## Checks for this correction

`xcodegen` 2.44.1 `generate` added only:

```xml
<key>ITSAppUsesNonExemptEncryption</key>
<false/>
```

to both checked-in plists. `PlistBuddy` prints `false` for that key in each file. `ITSEncryptionExportComplianceCode` does not exist. Release has no `NSAppTransportSecurity`. Debug `NSAllowsLocalNetworking` is `true`. The Xcode project file was unchanged.

```sh
xcodebuild -project ios/RiWorkRemote.xcodeproj -scheme RiWorkRemote \
  -configuration Debug -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath /tmp/riwork-export-compliance-dd CODE_SIGNING_ALLOWED=NO build
xcodebuild -project ios/RiWorkRemote.xcodeproj -scheme RiWorkRemote \
  -configuration Release -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath /tmp/riwork-export-compliance-dd CODE_SIGNING_ALLOWED=NO build
```

Both ended `** BUILD SUCCEEDED **` (Xcode 26.0.1). Built Info.plists:

| Configuration | `ITSAppUsesNonExemptEncryption` | `NSAppTransportSecurity` | compliance code |
| --- | --- | --- | --- |
| Debug-iphonesimulator | boolean `false` | `NSAllowsLocalNetworking` true | absent |
| Release-iphonesimulator | boolean `false` | absent | absent |

The earlier test run is unchanged and was not repeated: `swift test` 31 passed; simulator `xcodebuild test` 31 core + 22 app passed. No device, upload, or merge.

## Account-holder action that remains

None of Apple’s App Store Connect documents are required by the first row. There is no CCATS, French declaration, or `ITSEncryptionExportComplianceCode` to upload for this binary.

The complying page still says the account holder might have to submit a year-end self-classification report for exempt encryption: [How to file an Annual Self Classification Report](https://www.bis.doc.gov/index.php/policy-guidance/encryption/4-reports-and-reviews/a-annual-self-classification). This change does not file it and does not decide whether the BIS page covers this app.

The overview still assigns the account holder the EAR reading and the liability for an inaccurate exemption claim. This note does not replace that reading.

```json
{
  "task_id": "02201690-3e7f-4a79-bdad-c819fa4a6662",
  "project_id": "39832c2e-23a5-476d-aa8f-5ff34a02d314",
  "its_app_uses_non_exempt_encryption": false,
  "justified_by": "Apple documentation table row 1: encryption limited to the Apple operating system requires no App Store Connect documentation",
  "custom_protocol_makes_non_exempt": false,
  "official_text_for_that_rule": null,
  "its_encryption_export_compliance_code": null,
  "legal_attestation": false,
  "submitted": false,
  "apple_approval_claimed": false,
  "account_holder_action_remaining": "Read BIS annual self-classification guidance linked by Apple and decide whether to file. No App Store Connect encryption document is required for this binary.",
  "checks": {
    "checked_in_plists": "both boolean false; Debug ATS local networking true; Release ATS absent",
    "debug_release_build": "BUILD SUCCEEDED; built plists match"
  }
}
```
