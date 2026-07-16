/// A human-readable device name supplied by the user (RFC 017 §5.2).
///
/// Accepts any Unicode input; silently truncated at 256 Unicode scalar
/// values, matching desktop truffle-core (which counts Rust `char`s, i.e.
/// scalar values, not grapheme clusters).
public struct DeviceName: Hashable, Sendable, CustomStringConvertible {
    public static let maxScalars = 256

    public let value: String

    public init(_ raw: String) {
        if raw.unicodeScalars.count <= Self.maxScalars {
            self.value = raw
        } else {
            self.value = String(String.UnicodeScalarView(raw.unicodeScalars.prefix(Self.maxScalars)))
        }
    }

    public var description: String { value }
}
