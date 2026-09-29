# iOS export compliance inputs

This note records what the shipped RiWork iOS app uses for cryptography, and the App Store Connect inputs that are ready for an account holder. It is not a legal attestation, an export classification, or a submission. Apple has not reviewed or approved this app’s encryption documentation. No build was uploaded.

Retrieved 2026-09-29 from:

- [Overview of export compliance](https://developer.apple.com/help/app-store-connect/manage-app-information/overview-of-export-compliance)
- [Complying with Encryption Export Regulations](https://developer.apple.com/documentation/Security/complying-with-encryption-export-regulations)
- [ITSAppUsesNonExemptEncryption](https://developer.apple.com/documentation/bundleresources/information-property-list/itsappusesnonexemptencryption)
- [ITSEncryptionExportComplianceCode](https://developer.apple.com/documentation/bundleresources/information-property-list/itsencryptionexportcompliancecode)
- [Determine and upload app encryption documentation](https://developer.apple.com/help/app-store-connect/manage-app-information/determine-and-upload-app-encryption-documentation)
- [Export compliance documentation for encryption](https://developer.apple.com/help/app-store-connect/reference/app-information/export-compliance-documentation-for-encryption)
- [Provide export compliance information for beta builds](https://developer.apple.com/help/app-store-connect/test-a-beta-version/provide-export-compliance-information-for-beta-builds)

## Plist decision

`ITSAppUsesNonExemptEncryption` is absent from `ios/project.yml`, `ios/RiWorkRemote/Info.plist`, and `ios/RiWorkRemote/Info-Debug.plist`.

Apple’s key documentation says the Boolean is `NO` when the app, including linked third-party libraries, uses no encryption or only encryption that is exempt from export compliance requirements. It says the Boolean is `YES` when the app uses non-exempt encryption. If the value is `YES`, the same page says you typically also set `ITSEncryptionExportComplianceCode` to a code Apple provides after it reviews export compliance documentation. While the key is absent, App Store Connect asks the export compliance questionnaire on each upload.

That questionnaire has not been answered. No Apple-issued code exists. The overview page says an inaccurate exemption claim is the submitter’s responsibility, and the upload page limits that action to the Account Holder, Admin, or App Manager. Setting either Boolean from this repository would assert a result those pages still leave to that role.

`project.yml` remains the source for both checked-in plists. `postGenCommand` copies `Info.plist` to `Info-Debug.plist` and then adds `NSAppTransportSecurity` / `NSAllowsLocalNetworking`. Any future Boolean belongs in `info.properties` so both plists receive the same value. Add `ITSEncryptionExportComplianceCode` only with the string Apple shows next to approved documentation.

## Shipped cryptography

The iOS app target embeds the first-party `RiWorkCore` framework and links no third-party package. `ios/Package.swift` has no external dependencies. There is no CocoaPods, Carthage, or `Package.resolved` crypto module. Test and smoke targets are not in the app product.

| Use in the shipped app | API | Where | Role |
| --- | --- | --- | --- |
| Handshake authentication | CryptoKit `HMAC<SHA256>` | `ios/Core/SessionCrypto.swift` | 32-byte PSK authenticates `client_hello`, `server_hello`, and `client_finish` |
| Transcript hash and session id | CryptoKit `SHA256` | `ios/Core/SessionCrypto.swift` | Salt for key derivation; first 16 bytes are the session id |
| Traffic keys | CryptoKit `HKDF<SHA256>` | `ios/Core/SessionCrypto.swift` | Two independent 32-byte keys, info `riwork/v1/c2d` and `riwork/v1/d2c` |
| Payload confidentiality and integrity | CryptoKit `ChaChaPoly` (RFC 8439 ChaCha20-Poly1305) | `ios/Core/SessionCrypto.swift` | Seals and opens post-handshake JSON, including terminal text and commands |
| Client nonce | Security `SecRandomCopyBytes` | `ios/Core/SessionCrypto.swift` | 32-byte CSPRNG nonce per handshake |
| Stored pairing material | Security Keychain (`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, not synchronizable) | `ios/Core/KeychainStore.swift` | Pairing secret, relay token, and related saved state |
| Socket transport | `URLSession` / `URLSessionWebSocketTask` | `ios/Core/WebSocketTransport.swift` | Release pairing accepts `wss://`. Debug can allow `ws://` only for literal loopback when the Debug-only switch is on |

The v1 contract is specified in `docs/remote-protocol.md`. It states there is no ECDH and no forward secrecy. The same file cites RFC 8439 and RFC 5869. HMAC-SHA256 is the handshake MAC. The public repository `https://github.com/Rhein-Industries/riWork` contains that specification. Whether publication there meets the government’s “otherwise been published” wording is part of the EAR reading below, and this note does not decide it.

Debug’s checked-in plist adds `NSAllowsLocalNetworking`. Release’s checked-in plist has no `NSAppTransportSecurity` dictionary. The local-relay switch is compiled only in Debug (`ios/RiWorkRemote/PairingViews.swift`).

## How Apple’s pages describe the choice

Overview of export compliance, retrieved 2026-09-29:

> If your app uses, accesses, contains, implements, or incorporates encryption, and you intend to upload, test, and distribute it, you need to determine your export compliance requirements in App Store Connect.

The same page lists examples that require a determination: standard encryption algorithms, crypto functionality within Apple’s operating system, and proprietary or non-standard encryption algorithms. It quotes the US Government definition of non-standard cryptography as proprietary or unpublished cryptographic functionality, including encryption algorithms or protocols that have not been adopted or approved by a recognized standards body (IEEE, IETF, ISO, ITU, ETSI, 3GPP, TIA, and GSMA) and have not otherwise been published.

It also says:

> Please note that it’s your responsibility to review the Export Administration Regulation to determine whether your app's use of encryption requires a formal classification (Commodity Classification Automated Tracking System or CCATS) from BIS. you're responsible for all liabilities associated with misinterpretation of export regulations or claiming exemption inaccurately.

The overview’s next-step table:

| Scenario | Next step on that page |
| --- | --- |
| No export compliance documentation required | Update the app’s Info.plist so encryption questions are not repeated on each submission. |
| Export compliance documentation required | Submit the documentation in App Store Connect, attach the approved documentation to the build, then update Info.plist so the questions are not repeated. |

Complying with Encryption Export Regulations:

> Set the value to `NO` if your app—including any third-party libraries it links against—doesn’t use encryption, or if it only uses forms of encryption that are exempt from export compliance documentation requirements. Otherwise, set it to `YES`.

> Typically, the use of encryption that’s built into the operating system—for example, when your app makes HTTPS connections using URLSession—is exempt from export documentation upload requirements, whereas the use of proprietary encryption is not. To determine whether your use of encryption is considered exempt, see Determine and upload app encryption documentation.

> If your app uses exempt forms of encryption, you might alternatively be required to submit a year-end self-classification report to the U.S. government. (If you use non-exempt encryption and provide documentation to Apple, the self-classification report isn’t necessary.)

Apple’s documentation table at [Export compliance documentation for encryption](https://developer.apple.com/help/app-store-connect/reference/app-information/export-compliance-documentation-for-encryption):

| Encryption algorithm in use | Required documentation |
| --- | --- |
| Encryption limited to that within the Apple operating system | No documentation required in App Store Connect. |
| An industry standard algorithm not provided within the Apple operating system | French encryption declaration in App Store Connect. Required only if the app is distributed on the App Store in France. |
| Proprietary encryption algorithms not accepted by international standard bodies (such as IEEE, IETF, or ITU) | US CCATS, and a French encryption declaration if the app is distributed on the App Store in France. |

The overview separately says France controls secure storage, secure communications, and security anti-virus applications, with exemptions that include banking and medical applications. This app encrypts communications. France availability is not selected anywhere in this repository.

The algorithms in the table above are implemented by calling CryptoKit and Security, which ship in the operating system. The app does not link OpenSSL, libsodium, BoringSSL, or another bundled cipher. The session cipher still provides confidentiality for application payloads, so the use is wider than the HTTPS example on Apple’s complying page. The handshake protocol is a RiWork protocol documented in this repository, not an IETF protocol. Those facts are why the documentation-table row is not selected here.

## App Store Connect inputs

Do not upload a build, a CCATS, a French form, or a BIS report from this change. The pages below say who may answer, and where.

Required role, from Determine and upload app encryption documentation and from Provide export compliance information for beta builds: Account Holder, Admin, or App Manager.

Path when an app record exists:

1. App Store Connect → Apps → this app → App Information.
2. Next to App Encryption Documentation, add the documentation entry and answer the questions in the dialogs.
3. For a TestFlight build already marked Missing Compliance: TestFlight → platform build → Provide Export Compliance Information.

Facts available for those questions:

- The app uses encryption. The uses are OS Keychain, `URLSession` WebSocket TLS for `wss://`, and CryptoKit HMAC-SHA256, HKDF-SHA256, SHA-256, and ChaCha20-Poly1305 inside `RiWorkCore`.
- No third-party encryption library is linked.
- Proprietary ciphers are not implemented in the app. The payload cipher is RFC 8439 ChaCha20-Poly1305 through CryptoKit.
- Distribution countries, including France, are an availability choice in App Store Connect and are not fixed in the repo.
- No CCATS number, French declaration, annual self-classification confirmation, or `ITSEncryptionExportComplianceCode` is on file in this repo.

After that role saves an outcome:

- If the questions conclude that no documentation is required, Apple’s overview says to update Info.plist so later uploads skip the questions. The key page describes `NO` for encryption that is exempt from those requirements. Put the Boolean in `ios/project.yml` `info.properties`, regenerate, and keep Debug and Release identical for this key.
- If the questions require documentation, upload only the documents Apple names for the row that applies. Apple says review is case by case and, with complete information, expects about two business days. The code Apple then shows in App Encryption Documentation is the value for `ITSEncryptionExportComplianceCode`, together with `ITSAppUsesNonExemptEncryption` = `YES`.
- Apple’s complying page also points at a possible year-end self-classification report for exempt encryption: [How to file an Annual Self Classification Report](https://www.bis.doc.gov/index.php/policy-guidance/encryption/4-reports-and-reviews/a-annual-self-classification). This change does not file that report. The EAR page Apple links from the overview is [BIS encryption policy](https://www.bis.doc.gov/index.php/policy-guidance/encryption).

## What stays out of the plist until then

- `ITSAppUsesNonExemptEncryption`
- `ITSEncryptionExportComplianceCode`

Debug continues to differ only by `NSAllowsLocalNetworking`.
