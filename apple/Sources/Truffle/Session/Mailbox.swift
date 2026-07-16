/// A single-consumer async FIFO used by the in-memory test transports.
/// `put` returns false once the mailbox is finished (writes after close).
actor Mailbox<T: Sendable> {
    private var queue: [T] = []
    private var finished = false
    private var waiter: CheckedContinuation<T?, Never>?

    @discardableResult
    func put(_ value: T) -> Bool {
        guard !finished else { return false }
        if let w = waiter {
            waiter = nil
            w.resume(returning: value)
        } else {
            queue.append(value)
        }
        return true
    }

    /// Marks the mailbox finished. Queued values still drain before `take`
    /// returns nil.
    func finish() {
        guard !finished else { return }
        finished = true
        if let w = waiter {
            waiter = nil
            w.resume(returning: nil)
        }
    }

    var isFinished: Bool { finished }

    /// Next value, or nil once finished and drained. Single consumer only.
    /// Cancellation-aware: a cancelled `take` resumes with nil instead of
    /// blocking forever (deadlines race against this in task groups).
    func take() async -> T? {
        if !queue.isEmpty { return queue.removeFirst() }
        if finished { return nil }
        return await withTaskCancellationHandler {
            await withCheckedContinuation { (c: CheckedContinuation<T?, Never>) in
                assert(waiter == nil, "Mailbox supports a single consumer")
                if Task.isCancelled {
                    c.resume(returning: nil)
                    return
                }
                waiter = c
            }
        } onCancel: {
            Task { await self.resumeWaiterOnCancel() }
        }
    }

    private func resumeWaiterOnCancel() {
        if let w = waiter {
            waiter = nil
            w.resume(returning: nil)
        }
    }
}
