import Foundation
import Testing

@testable import Truffle

// MARK: - AppId (parity with truffle-core identity.rs)

@Suite struct AppIdTests {
    @Test func acceptsValidIds() throws {
        for ok in ["ab", "chat", "field-tools", "a1", "app-2x", "a" + String(repeating: "b", count: 31)] {
            _ = try AppId(parsing: ok)
        }
    }

    @Test func rejectsInvalidIds() {
        let bad = [
            "", "a",                       // too short
            "a" + String(repeating: "b", count: 32),  // too long (33)
            "1app", "-app", "Aapp",        // bad first char
            "app_x", "app x", "appé",      // bad charset
            "app-",                        // trailing hyphen (desktop rule)
        ]
        for s in bad {
            #expect(throws: MeshError.invalidAppId(s)) { try AppId(parsing: s) }
        }
    }
}

// MARK: - DeviceId (ULID)

@Suite struct DeviceIdTests {
    @Test func generateProducesValidUlid() throws {
        let id = DeviceId.generate()
        #expect(id.value.count == 26)
        let reparsed = try DeviceId(parsing: id.value)
        #expect(reparsed == id)
    }

    @Test func parsesKnownRustUlid() throws {
        // ULID from truffle-core hello.rs tests.
        let id = try DeviceId(parsing: "01J4K9M2Z8AB3RNYQPW6H5TC0X")
        #expect(id.value == "01J4K9M2Z8AB3RNYQPW6H5TC0X")
    }

    @Test func parseIsCaseInsensitiveCanonicalUppercase() throws {
        let id = try DeviceId(parsing: "01j4k9m2z8ab3rnyqpw6h5tc0x")
        #expect(id.value == "01J4K9M2Z8AB3RNYQPW6H5TC0X")
    }

    @Test func rejectsInvalidUlids() {
        for bad in ["", "0123", "01J4K9M2Z8AB3RNYQPW6H5TC0I", "81J4K9M2Z8AB3RNYQPW6H5TC0X",
                    String(repeating: "0", count: 25), String(repeating: "0", count: 27)] {
            #expect(throws: MeshError.self) { try DeviceId(parsing: bad) }
        }
    }

    @Test func timestampOrderingIsPreserved() {
        // ULIDs generated at strictly increasing millisecond timestamps sort
        // lexicographically (Crockford base32 preserves numeric order).
        let a = DeviceId.generate(now: Date(timeIntervalSince1970: 1_000))
        let b = DeviceId.generate(now: Date(timeIntervalSince1970: 2_000))
        #expect(a.value < b.value)
    }

    @Test func persistenceRoundTrips() throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("truffle-test-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: dir) }

        let first = try DeviceId.loadOrCreate(stateDirectory: dir)
        let second = try DeviceId.loadOrCreate(stateDirectory: dir)
        #expect(first == second)

        let onDisk = try String(
            contentsOf: dir.appendingPathComponent(DeviceId.stateFileName), encoding: .utf8)
        #expect(onDisk.trimmingCharacters(in: .whitespacesAndNewlines) == first.value)
    }

    @Test func corruptPersistedIdThrowsInsteadOfRotating() throws {
        // Desktop parity (node.rs "device-id.txt contains an invalid
        // ULID"): durable identity must fail loudly, never silently mint a
        // new identity over a corrupt file.
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("truffle-test-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: dir) }
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let file = dir.appendingPathComponent(DeviceId.stateFileName)
        try Data("not-a-ulid".utf8).write(to: file)

        #expect(throws: MeshError.self) {
            _ = try DeviceId.loadOrCreate(stateDirectory: dir)
        }
        // The corrupt file is untouched — no rotation happened.
        let onDisk = try String(contentsOf: file, encoding: .utf8)
        #expect(onDisk == "not-a-ulid")
    }
}

// MARK: - DeviceName

@Suite struct DeviceNameTests {
    @Test func passesShortNamesThrough() {
        #expect(DeviceName("Alice's MacBook Pro").value == "Alice's MacBook Pro")
    }

    @Test func truncatesAt256Scalars() {
        let long = String(repeating: "x", count: 300)
        #expect(DeviceName(long).value.count == 256)
    }
}

// MARK: - Slug / hostname (pipeline parity with identity.rs §5.3)

@Suite struct SlugTests {
    @Test func alicesMacbookPro() {
        // Same expectation as the Rust test slug_alice_s_macbook_pro.
        #expect(Hostname.slug("Alice's MacBook Pro", budget: 63) == "alice-s-macbook-pro")
    }

    @Test func collapsesAndTrimsHyphens() {
        #expect(Hostname.slug("--a  b--", budget: 63) == "a-b")
        #expect(Hostname.slug("a———b", budget: 63) == "a-b")
    }

    @Test func lowercasesAndStripsDiacritics() {
        #expect(Hostname.slug("Café Über", budget: 63) == "cafe-uber")
    }

    @Test func unmappableInputFallsBackToHash() {
        // Pure-emoji input produces no ASCII; the fallback hash kicks in.
        let s = Hostname.slug("🦄🦄🦄", budget: 63)
        #expect(!s.isEmpty)
        #expect(s.count >= 2)
        #expect(s.allSatisfy { ($0.isLowercase && $0.isASCII) || $0.isNumber || $0 == "-" })
    }

    @Test func respectsBudget() {
        let s = Hostname.slug(String(repeating: "a", count: 100), budget: 10)
        #expect(s.count == 10)
    }

    @Test func budgetZeroIsEmpty() {
        #expect(Hostname.slug("anything", budget: 0) == "")
    }

    @Test func composesHostnameWithinDnsLimit() throws {
        let appId = try AppId(parsing: "field-tools")
        let host = Hostname.tailscaleHostname(
            appId: appId, deviceName: DeviceName("Alice's iPhone 15 Pro Max With A Very Long Name Indeed"))
        #expect(host.hasPrefix("truffle-field-tools-"))
        #expect(host.count <= Hostname.dnsLabelLimit)
        #expect(Hostname.isAppPeer(hostname: host, appId: "field-tools"))
    }

    @Test func isAppPeerParity() {
        // Mirrors provider.rs::is_app_peer semantics.
        #expect(Hostname.isAppPeer(hostname: "truffle-demo-dev", appId: "demo"))
        #expect(!Hostname.isAppPeer(hostname: "truffle-demo-", appId: "demo"))   // empty slug
        #expect(!Hostname.isAppPeer(hostname: "truffle-demo", appId: "demo"))
        #expect(!Hostname.isAppPeer(hostname: "truffle-other-dev", appId: "demo"))
        #expect(!Hostname.isAppPeer(hostname: "laptop", appId: "demo"))
    }
}
