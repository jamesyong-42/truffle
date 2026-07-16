/// An application identifier that defines the namespace two nodes must share
/// in order to see each other as peers (RFC 017 §5.1).
///
/// Valid `AppId`s match `^[a-z][a-z0-9-]{1,31}$`:
/// - first character: lowercase ASCII letter
/// - remaining: lowercase ASCII letters, digits, or `-`
/// - length: 2–32 characters
/// - trailing hyphen rejected (would produce ambiguous
///   `truffle-{appId}-{slug}` hostnames with double separators)
///
/// Mirrors `truffle-core::identity::AppId` including the trailing-hyphen rule.
public struct AppId: Hashable, Sendable, CustomStringConvertible {
    public let value: String

    public init(parsing raw: String) throws {
        guard Self.isValid(raw) else {
            throw MeshError.invalidAppId(raw)
        }
        self.value = raw
    }

    public var description: String { value }

    static func isValid(_ s: String) -> Bool {
        let bytes = Array(s.utf8)
        guard (2...32).contains(bytes.count) else { return false }
        guard bytes[0] >= UInt8(ascii: "a"), bytes[0] <= UInt8(ascii: "z") else {
            return false
        }
        for b in bytes.dropFirst() {
            let lower = b >= UInt8(ascii: "a") && b <= UInt8(ascii: "z")
            let digit = b >= UInt8(ascii: "0") && b <= UInt8(ascii: "9")
            guard lower || digit || b == UInt8(ascii: "-") else { return false }
        }
        // Trailing hyphen is reserved (matches desktop validation).
        if bytes.last == UInt8(ascii: "-") { return false }
        return true
    }
}
