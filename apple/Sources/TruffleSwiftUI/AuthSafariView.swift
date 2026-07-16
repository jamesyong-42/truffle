#if os(iOS)
    import SafariServices
    import SwiftUI

    /// In-app Safari sheet for the interactive Tailscale login
    /// (RFC 024 §9 — `SFSafariViewController` is the recommended
    /// presentation). Present it from `MeshModel.authURL`:
    ///
    /// ```swift
    /// .sheet(item: $model.authURL.map(AuthPage.init)) { page in
    ///     AuthSafariView(url: page.url) { Task { await model.refresh() } }
    /// }
    /// ```
    public struct AuthSafariView: UIViewControllerRepresentable {
        private let url: URL
        private let onDismiss: () -> Void

        public init(url: URL, onDismiss: @escaping () -> Void = {}) {
            self.url = url
            self.onDismiss = onDismiss
        }

        public func makeUIViewController(context: Context) -> SFSafariViewController {
            let controller = SFSafariViewController(url: url)
            controller.delegate = context.coordinator
            return controller
        }

        public func updateUIViewController(_: SFSafariViewController, context: Context) {}

        public func makeCoordinator() -> Coordinator {
            Coordinator(onDismiss: onDismiss)
        }

        public final class Coordinator: NSObject, SFSafariViewControllerDelegate {
            private let onDismiss: () -> Void

            init(onDismiss: @escaping () -> Void) {
                self.onDismiss = onDismiss
            }

            public func safariViewControllerDidFinish(_: SFSafariViewController) {
                onDismiss()
            }
        }
    }

    /// Identifiable wrapper so an auth URL can drive `.sheet(item:)`.
    public struct AuthPage: Identifiable, Sendable {
        public let url: URL
        public var id: String { url.absoluteString }

        public init(url: URL) {
            self.url = url
        }
    }
#endif
