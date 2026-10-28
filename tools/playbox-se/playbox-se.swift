// playbox-se: sign with a Secure Enclave P-256 key, for Playbox platform attestation (macOS only).
//
//   playbox-se pub  --key FILE          print the device public key, creating the key if FILE is missing
//   playbox-se sign --key FILE --msg HEX  sign HEX with the key (ECDSA-SHA256); prints `pub HEX` and `sig HEX`
//
// FILE holds CryptoKit's opaque key handle. It only works on this Mac's Secure Enclave, and does not
// reveal the private key. CryptoKit keys are used instead of keychain keys because keychain-stored
// Secure Enclave keys need an entitlement that a plain command-line tool does not have.

import CryptoKit
import Foundation

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}

func hex(_ d: Data) -> String { d.map { String(format: "%02x", $0) }.joined() }

func unhex(_ s: String) -> Data? {
    guard s.count % 2 == 0 else { return nil }
    var out = Data()
    var i = s.startIndex
    while i < s.endIndex {
        let j = s.index(i, offsetBy: 2)
        guard let b = UInt8(s[i..<j], radix: 16) else { return nil }
        out.append(b)
        i = j
    }
    return out
}

func loadOrCreate(_ path: String) -> SecureEnclave.P256.Signing.PrivateKey {
    guard SecureEnclave.isAvailable else { fail("no Secure Enclave on this machine") }
    if let blob = FileManager.default.contents(atPath: path) {
        do { return try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: blob) }
        catch { fail("cannot load key handle \(path): \(error)") }
    }
    do {
        let key = try SecureEnclave.P256.Signing.PrivateKey()
        let url = URL(fileURLWithPath: path)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        guard FileManager.default.createFile(atPath: path, contents: key.dataRepresentation, attributes: [.posixPermissions: 0o600]) else {
            fail("cannot write key handle to \(path)")
        }
        return key
    } catch {
        fail("cannot create Secure Enclave key: \(error)")
    }
}

let args = CommandLine.arguments
func option(_ name: String) -> String? {
    guard let i = args.firstIndex(of: name), i + 1 < args.count else { return nil }
    return args[i + 1]
}

guard args.count >= 2, let keyPath = option("--key") else { fail("usage: playbox-se pub|sign --key FILE [--msg HEX]") }
let key = loadOrCreate(keyPath)
let pub = hex(key.publicKey.x963Representation)

switch args[1] {
case "pub":
    print("pub \(pub)")
case "sign":
    guard let m = option("--msg"), let msg = unhex(m), !msg.isEmpty else { fail("--msg must be non-empty hex") }
    do {
        let sig = try key.signature(for: msg)
        print("pub \(pub)")
        print("sig \(hex(sig.rawRepresentation))")
    } catch {
        fail("signing failed: \(error)")
    }
default:
    fail("unknown command \(args[1])")
}
