import SwiftUI
import Truffle

struct ChatView: View {
    let peer: Peer
    let world: DemoWorld
    let chat: ChatStore

    @State private var draft = ""

    private var entries: [ChatStore.Entry] {
        chat.threads[peer.tailscaleId] ?? []
    }

    var body: some View {
        VStack(spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 8) {
                        ForEach(entries) { entry in
                            MessageBubble(entry: entry)
                        }
                    }
                    .padding()
                }
                .onChange(of: entries.count) {
                    if let last = entries.last {
                        withAnimation {
                            proxy.scrollTo(last.id, anchor: .bottom)
                        }
                    }
                }
            }

            if let error = chat.lastError {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .padding(.horizontal)
            }

            HStack {
                TextField("Message \(peer.displayName)", text: $draft)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(send)
                Button(action: send) {
                    Image(systemName: "paperplane.fill")
                }
                .disabled(draft.trimmingCharacters(in: .whitespaces).isEmpty)
            }
            .padding()
        }
        .navigationTitle(peer.displayName)
        .navigationBarTitleDisplayMode(.inline)
    }

    private func send() {
        let text = draft.trimmingCharacters(in: .whitespaces)
        guard !text.isEmpty, let node = world.model.node else { return }
        draft = ""
        Task {
            await chat.send(text, to: peer, via: node)
        }
    }
}

struct MessageBubble: View {
    let entry: ChatStore.Entry

    var body: some View {
        HStack {
            if entry.isMe { Spacer(minLength: 48) }
            Text(entry.text)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(
                    entry.isMe ? Color.accentColor : Color(.secondarySystemBackground),
                    in: RoundedRectangle(cornerRadius: 16))
                .foregroundStyle(entry.isMe ? .white : .primary)
            if !entry.isMe { Spacer(minLength: 48) }
        }
        .id(entry.id)
    }
}
