import Foundation

/// A stable per-device identifier, formatted as a 26-character Crockford
/// base32 ULID (RFC 017 §5.4).
///
/// Mirrors `truffle-core::identity::DeviceId`. Truffle persists the ULID
/// under `{stateDirectory}/device-id.txt` (same file name as desktop) so a
/// device keeps its identity across restarts, Tailscale re-auths, and
/// ephemeral-node rotations (RFC 024 §7.1).
public struct DeviceId: Hashable, Sendable, CustomStringConvertible {
    public let value: String

    /// Crockford base32 alphabet (no I, L, O, U).
    private static let alphabet = Array("0123456789ABCDEFGHJKMNPQRSTVWXYZ")

    private static let decodeMap: [Character: Int] = {
        var map: [Character: Int] = [:]
        for (i, c) in alphabet.enumerated() {
            map[c] = i
            map[Character(c.lowercased())] = i
        }
        return map
    }()

    // MARK: Construction

    /// Generate a new random ULID: 48-bit millisecond timestamp + 80 random bits.
    public static func generate(now: Date = Date()) -> DeviceId {
        var bytes = [UInt8](repeating: 0, count: 16)
        let millis = UInt64(now.timeIntervalSince1970 * 1000)
        for i in 0..<6 {
            bytes[i] = UInt8((millis >> (8 * UInt64(5 - i))) & 0xFF)
        }
        var rng = SystemRandomNumberGenerator()
        for i in 6..<16 {
            bytes[i] = UInt8.random(in: .min ... .max, using: &rng)
        }
        return DeviceId(unchecked: Self.encode(bytes))
    }

    /// Parse a string as a ULID, rejecting anything that is not a valid
    /// 26-character Crockford base32 ULID. Case-insensitive; canonical form
    /// is uppercase (matches the Rust `ulid` crate behavior).
    public init(parsing raw: String) throws {
        guard raw.count == 26 else {
            throw MeshError.invalidPayload("invalid device_id '\(raw)': not a valid ULID")
        }
        var values: [Int] = []
        values.reserveCapacity(26)
        for c in raw {
            guard let v = Self.decodeMap[c] else {
                throw MeshError.invalidPayload("invalid device_id '\(raw)': not a valid ULID")
            }
            values.append(v)
        }
        // 26 × 5 bits = 130 bits; the top 2 bits must be zero for the value
        // to fit in 128 bits, i.e. the first character must be 0–7.
        guard values[0] < 8 else {
            throw MeshError.invalidPayload("invalid device_id '\(raw)': overflows 128 bits")
        }
        self.value = String(values.map { Self.alphabet[$0] })
    }

    private init(unchecked value: String) {
        self.value = value
    }

    public var description: String { value }

    // MARK: Persistence

    /// The file (relative to the state directory) where the device ULID is
    /// persisted. Matches desktop truffle-core (`{state_dir}/device-id.txt`).
    public static let stateFileName = "device-id.txt"

    /// Load the persisted device ULID from `stateDirectory`, or generate and
    /// persist a fresh one. Creates the directory if needed and excludes it
    /// from backup (RFC 024 §14 — the directory also holds Tailscale keys).
    ///
    /// A file that exists but does not parse is an ERROR, never a silent
    /// rotation — matching desktop (`node.rs`: "device-id.txt contains an
    /// invalid ULID"). Durable identity must fail loudly (RFC 024 §7.1).
    public static func loadOrCreate(stateDirectory: URL) throws -> DeviceId {
        let fm = FileManager.default
        let file = stateDirectory.appendingPathComponent(stateFileName)
        if fm.fileExists(atPath: file.path) {
            let data = try Data(contentsOf: file)
            let text = String(decoding: data, as: UTF8.self)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            do {
                return try DeviceId(parsing: text)
            } catch {
                throw MeshError.invalidPayload(
                    "device-id.txt at \(file.path) contains an invalid ULID: '\(text)'")
            }
        }
        try fm.createDirectory(at: stateDirectory, withIntermediateDirectories: true)
        excludeFromBackup(stateDirectory)
        let fresh = DeviceId.generate()
        try Data(fresh.value.utf8).write(to: file, options: .atomic)
        return fresh
    }

    /// Best-effort `NSURLIsExcludedFromBackupKey` on the state directory —
    /// private keys and identity must not round-trip through iCloud/Finder
    /// backups (RFC 024 §14). Best-effort because some volumes (e.g. tmpfs
    /// in tests) do not support the resource key.
    private static func excludeFromBackup(_ directory: URL) {
        var url = directory
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? url.setResourceValues(values)
    }

    // MARK: Codec

    /// Encode 16 bytes as a 26-character Crockford base32 ULID string.
    /// The 128 data bits are prefixed with 2 zero bits to fill 26 × 5 bits.
    static func encode(_ bytes: [UInt8]) -> String {
        precondition(bytes.count == 16)
        var out = [Character]()
        out.reserveCapacity(26)
        for group in 0..<26 {
            var v = 0
            for k in 0..<5 {
                let bit130 = group * 5 + k
                let bit128 = bit130 - 2
                let bit: Int
                if bit128 < 0 {
                    bit = 0
                } else {
                    bit = Int((bytes[bit128 / 8] >> (7 - (bit128 % 8))) & 1)
                }
                v = (v << 1) | bit
            }
            out.append(alphabet[v])
        }
        return String(out)
    }
}
