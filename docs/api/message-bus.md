# MessageBus (Legacy)

::: warning
This documents the **old Node.js API**. In v2, namespace-based messaging is handled by `Node::send()`, `Node::broadcast()`, and `Node::subscribe()` in Rust. See the [Architecture guide](/guide/architecture#node-api-public-entry-point).
:::

The message bus provides namespace-based pub/sub messaging across devices.

## Getting the Bus

```typescript
const bus = node.getMessageBus();
```

## Methods

### `subscribe(namespace, handler): () => void`

Subscribe to messages in a namespace. Returns an unsubscribe function.

```typescript
const unsubscribe = bus.subscribe('chat', (message) => {
  console.log(message.from, message.type, message.payload);
});

// Later:
unsubscribe();
```

### `publish(targetId, namespace, type, payload): boolean`

Send a message to a specific device. Returns `true` if sent.

```typescript
bus.publish('device-123', 'chat', 'message', { text: 'Hello!' });
```

### `broadcast(namespace, type, payload): void`

Broadcast a message to all connected devices.

```typescript
bus.broadcast('chat', 'message', { text: 'Hello everyone!' });
```

## BusMessage

```typescript
interface BusMessage {
  from: string | undefined;
  namespace: string;
  type: string;
  payload: unknown;
}
```

## Patterns

### Request/Response

```typescript
// Responder
bus.subscribe('rpc', (msg) => {
  if (msg.type === 'ping' && msg.from) {
    bus.publish(msg.from, 'rpc', 'pong', { time: Date.now() });
  }
});

// Requester
bus.broadcast('rpc', 'ping', {});
bus.subscribe('rpc', (msg) => {
  if (msg.type === 'pong') {
    console.log('Response from', msg.from, msg.payload);
  }
});
```
