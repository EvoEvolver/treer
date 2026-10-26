// macOS publisher custody. No private-key file, argv, stdout or export operation.
// Compile to a stable, owner-only path before creating the Keychain item.
import Foundation
import CryptoKit
import Security

let service = "ai.treer.release-signing.ed25519.official.v1"
let account = "releases.treer.ai"
func refuse(_ code: String) -> Never {
    FileHandle.standardError.write(Data((code + "\n").utf8)); exit(1)
}
func check(_ status: OSStatus, _ code: String) {
    if status != errSecSuccess { refuse("\(code):\(status)") }
}
func query() -> [String: Any] {
    [kSecClass as String: kSecClassGenericPassword,
     kSecAttrService as String: service, kSecAttrAccount as String: account,
     kSecAttrSynchronizable as String: false,
     kSecUseAuthenticationUI as String: kSecUseAuthenticationUIFail]
}
func storedKey() -> Curve25519.Signing.PrivateKey {
    var q = query(); q[kSecReturnData as String] = true
    q[kSecMatchLimit as String] = kSecMatchLimitOne
    var found: CFTypeRef?
    check(SecItemCopyMatching(q as CFDictionary, &found), "custody_read_refused")
    guard let bytes = found as? Data, bytes.count == 32,
          let key = try? Curve25519.Signing.PrivateKey(rawRepresentation: bytes)
    else { refuse("custody_key_invalid") }
    return key
}
func publicRecord(_ key: Curve25519.Signing.PublicKey) -> [String: Any] {
    // RFC 8410 Ed25519 SubjectPublicKeyInfo, absent algorithm parameters.
    let der = Data([0x30,0x2a,0x30,0x05,0x06,0x03,0x2b,0x65,0x70,0x03,0x21,0x00]) + key.rawRepresentation
    let encoded = der.base64EncodedString()
    let pem = "-----BEGIN PUBLIC KEY-----\n" + encoded + "\n-----END PUBLIC KEY-----\n"
    let digest = SHA256.hash(data: der).map { String(format: "%02x", $0) }.joined()
    return ["schema": "treer.release-authority/v1", "algorithm": "ed25519",
            "public_pem": pem, "spki_der_sha256": digest,
            "key_id": String(digest.prefix(16)), "custody": "macOS login Keychain; executable ACL; not hardware-backed"]
}
func emit(_ record: [String: Any]) {
    guard let bytes = try? JSONSerialization.data(withJSONObject: record, options: [.sortedKeys]) else { refuse("output_invalid") }
    FileHandle.standardOutput.write(bytes + Data([10]))
}
guard CommandLine.arguments.count == 2 else { refuse("use_status_create_public_sign_selftest") }
switch CommandLine.arguments[1] {
case "status":
    var q = query(); q[kSecReturnAttributes as String] = true
    q[kSecMatchLimit as String] = kSecMatchLimitOne
    let status = SecItemCopyMatching(q as CFDictionary, nil)
    if status == errSecItemNotFound { emit(["present": false]) }
    else { check(status, "custody_status_refused"); emit(["present": true]) }
case "create":
    // Check first, then rely on Keychain's atomic duplicate-item refusal.
    let status = SecItemCopyMatching(query() as CFDictionary, nil)
    guard status == errSecItemNotFound else { refuse("custody_exists_or_unknown:\(status)") }
    var application: SecTrustedApplication?
    check(SecTrustedApplicationCreateFromPath(CommandLine.arguments[0], &application), "custody_application_refused")
    guard let application else { refuse("custody_application_missing") }
    var access: SecAccess?
    check(SecAccessCreate("Treer official release signer" as CFString, [application] as CFArray, &access), "custody_acl_refused")
    guard let access else { refuse("custody_acl_missing") }
    let key = Curve25519.Signing.PrivateKey()
    var item = query(); item.removeValue(forKey: kSecUseAuthenticationUI as String)
    item[kSecAttrAccess as String] = access
    item[kSecAttrLabel as String] = "Treer official Ed25519 release authority"
    item[kSecValueData as String] = key.rawRepresentation
    check(SecItemAdd(item as CFDictionary, nil), "custody_create_refused")
    // Read back from durable custody before publishing the public record.
    let loaded = storedKey()
    guard loaded.publicKey.rawRepresentation == key.publicKey.rawRepresentation else { refuse("custody_readback_mismatch") }
    emit(publicRecord(loaded.publicKey))
case "public": emit(publicRecord(storedKey().publicKey))
case "sign":
    let bytes = FileHandle.standardInput.readData(ofLength: 1024 * 1024 + 1)
    guard bytes.count <= 1024 * 1024 else { refuse("signing_input_oversized") }
    let key = storedKey()
    guard let signature = try? key.signature(for: bytes) else { refuse("signing_failed") }
    let record = publicRecord(key.publicKey)
    emit(["schema_version": 1, "algorithm": "ed25519", "key_id": record["key_id"]!, "signature": signature.base64EncodedString()])
case "selftest":
    // In-memory mechanism check only: does not create a Keychain item.
    let key = Curve25519.Signing.PrivateKey(), bytes = Data("Treer signer control".utf8)
    guard let signature = try? key.signature(for: bytes),
          key.publicKey.isValidSignature(signature, for: bytes),
          !key.publicKey.isValidSignature(signature, for: Data("changed".utf8))
    else { refuse("selftest_failed") }
    emit(["selftest": "PASS", "custody_mutated": false])
default: refuse("unsupported_operation")
}
