# iOS export compliance inputs

This note matches the shipped iOS binary to Apple’s documentation table. It is not a legal attestation, an EAR classification, a filing, or an App Store submission. Apple has not approved the app.

Pages retrieved 2026-09-29:

- [Export compliance documentation for encryption](https://developer.apple.com/help/app-store-connect/reference/app-information/export-compliance-documentation-for-encryption)
- [Overview of export compliance](https://developer.apple.com/help/app-store-connect/manage-app-information/overview-of-export-compliance)
- [Complying with Encryption Export Regulations](https://developer.apple.com/documentation/Security/complying-with-encryption-export-regulations)
- [ITSAppUsesNonExemptEncryption](https://developer.apple.com/documentation/bundleresources/information-property-list/itsappusesnonexemptencryption)
- [ITSEncryptionExportComplianceCode](https://developer.apple.com/documentation/bundleresources/information-property-list/itsencryptionexportcompliancecode)

## Correction

An earlier draft of this note said the published RiWork message format spanned more than one row of Apple’s table. The cited pages do not say that. A custom application protocol is not, by itself, a reason to leave the first row when the encryption in the app is limited to the Apple operating system.

## Row that matches the binary

[Export compliance documentation for encryption](https://developer.apple.com/help/app-store-connect/reference/app-information/export-compliance-documentation-for-encryption) says, as its first row:

> Your app uses encryption limited to that within the Apple operating system
>
> No documentation required in App Store Connect.

The same table’s other rows are an industry-standard algorithm **not provided within** the Apple operating system, and proprietary encryption algorithms not accepted by international standard bodies such as IEEE, IETF, or ITU. The French declaration on those two rows is required only when the app is distributed on the App Store in France.

Debug and Release `otool -L` of the built app shows `RiWorkCore` linking `/System/Library/Frameworks/CryptoKit.framework/CryptoKit` and `/System/Library/Frameworks/Security.framework/Security`. The app executable links that framework and system frameworks, including Foundation’s `URLSession`. There is no `libcrypto`, OpenSSL, libsodium, or BoringSSL load command. `ios/Package.swift` has no external dependencies.

Calls in the app product are CryptoKit `HMAC<SHA256>`, `SHA256`, `HKDF<SHA256>`, and `ChaChaPoly`, Security `SecRandomCopyBytes` and Keychain, and `URLSessionWebSocketTask`. ChaCha20-Poly1305 is the RFC 8439 construction CryptoKit implements. The message format in `docs/remote-protocol.md` chooses nonces, associated data, and counters. It does not add an encryption implementation.

[Complying with Encryption Export Regulations](https://developer.apple.com/documentation/Security/complying-with-encryption-export-regulations) describes the same line:

> Typically, the use of encryption that’s built into the operating system—for example, when your app makes HTTPS connections using URLSession—is exempt from export documentation upload requirements, whereas the use of proprietary encryption is not.

The example is HTTPS. The contrast is proprietary encryption. The sentence does not say that other calls into operating-system crypto are outside the first row.

The overview’s definition of non-standard cryptography is:

> any implementation of “cryptography” involving the incorporation or use of proprietary or unpublished cryptographic functionality, including encryption algorithms or protocols that have not been adopted or approved by a duly recognized international standards body (e.g., IEEE, IETF, ISO, ITU, ETSI, 3GPP, TIA, and GSMA) and haven’t otherwise been published.

That definition is attached to proprietary or non-standard encryption. It does not say that a published application format which only calls operating-system implementations of standard algorithms requires App Store Connect documentation.

## Plist value

`ITSAppUsesNonExemptEncryption` is `false` in `ios/project.yml`, `ios/RiWorkRemote/Info.plist`, and `ios/RiWorkRemote/Info-Debug.plist`.

The key page says:

> Set the value for this key to `NO` in your app’s Information Property List file to indicate that your app—including any third-party libraries you link against—either uses no encryption, or only uses encryption that’s exempt from export compliance requirements, as described in Overview of export compliance. Set the value to `YES` to indicate that your app uses non-exempt encryption.

The complying page says to set `NO` when the app uses only forms of encryption that are exempt from export compliance documentation requirements, and otherwise to set `YES`. The first documentation row says no documentation is required. `YES` is the value that page pairs with `ITSEncryptionExportComplianceCode`, a code Apple provides after it reviews documentation. That code is not set.

`postGenCommand` copies `Info.plist` to `Info-Debug.plist` and then adds `NSAllowsLocalNetworking`. Debug and Release carry the same Boolean. Release has no `NSAppTransportSecurity` dictionary.

## What remains for the account holder

No App Store Connect encryption document follows from the first row. This change does not upload a build, a CCATS, a French declaration, or a compliance code.

Apple’s complying page still says:

> If your app uses exempt forms of encryption, you might alternatively be required to submit a year-end self-classification report to the U.S. government. (If you use non-exempt encryption and provide documentation to Apple, the self-classification report isn’t necessary.)

The page links [How to file an Annual Self Classification Report](https://www.bis.doc.gov/index.php/policy-guidance/encryption/4-reports-and-reviews/a-annual-self-classification). This note does not file that report and does not decide whether this app is one of the products the BIS page covers. That reading is the account holder’s.

The overview also says it is the reader’s responsibility to review the Export Administration Regulations and that an inaccurate exemption claim is the submitter’s liability. This note does not replace that review. The overview’s paragraph on French controls does not add a document to the first row; the documentation table attaches the French declaration only to the other two rows, and only if the app is distributed in France.
