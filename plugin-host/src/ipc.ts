// IPC with James: length-prefixed JSON frames over the socket passed as file descriptor 3.
// Frame shapes are documented in crates/james-plugins/src/ipc.rs.

import net from 'node:net';

export const PROTOCOL_VERSION = 1;
const MAX_FRAME = 16 << 20;

export type Frame =
  | { t: 'req'; id: number; method: string; params?: unknown }
  | { t: 'res'; id: number; result?: unknown; error?: string }
  | { t: 'evt'; name: string; data?: unknown };

export type RequestHandler = (method: string, params: any) => Promise<unknown>;
export type EventHandler = (name: string, data: any) => void;

export class Connection {
  #socket: net.Socket;
  #buffer: Buffer = Buffer.alloc(0);
  #nextId = 1;
  #pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void; timer: NodeJS.Timeout }>();
  #onRequest: RequestHandler;
  #onEvent: EventHandler;

  constructor(socket: net.Socket, onRequest: RequestHandler, onEvent: EventHandler, onClose: () => void) {
    this.#socket = socket;
    this.#onRequest = onRequest;
    this.#onEvent = onEvent;
    socket.on('data', (chunk) => this.#receive(chunk));
    socket.on('close', () => {
      for (const p of this.#pending.values()) {
        clearTimeout(p.timer);
        p.reject(new Error('connection to James closed'));
      }
      this.#pending.clear();
      onClose();
    });
    socket.on('error', () => socket.destroy());
  }

  static fromFd(fd: number, onRequest: RequestHandler, onEvent: EventHandler, onClose: () => void): Connection {
    return new Connection(new net.Socket({ fd, readable: true, writable: true }), onRequest, onEvent, onClose);
  }

  request(method: string, params: unknown, timeoutMs = 15_000): Promise<any> {
    const id = this.#nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error(`James did not answer '${method}' in time`));
      }, timeoutMs);
      this.#pending.set(id, { resolve, reject, timer });
      this.#send({ t: 'req', id, method, params });
    });
  }

  notify(name: string, data: unknown): void {
    this.#send({ t: 'evt', name, data });
  }

  #send(frame: Frame): void {
    const body = Buffer.from(JSON.stringify(frame));
    if (body.length > MAX_FRAME) {
      if (frame.t === 'res') this.#send({ t: 'res', id: frame.id, error: 'response too large' });
      return;
    }
    const header = Buffer.alloc(4);
    header.writeUInt32BE(body.length);
    this.#socket.write(Buffer.concat([header, body]));
  }

  #receive(chunk: Buffer): void {
    this.#buffer = this.#buffer.length ? Buffer.concat([this.#buffer, chunk]) : chunk;
    while (this.#buffer.length >= 4) {
      const length = this.#buffer.readUInt32BE(0);
      if (length > MAX_FRAME) {
        this.#socket.destroy(new Error('frame too large'));
        return;
      }
      if (this.#buffer.length < 4 + length) return;
      const body = this.#buffer.subarray(4, 4 + length);
      this.#buffer = this.#buffer.subarray(4 + length);
      let frame: Frame;
      try {
        frame = JSON.parse(body.toString('utf8'));
      } catch {
        this.#socket.destroy(new Error('invalid frame'));
        return;
      }
      this.#dispatch(frame);
    }
  }

  #dispatch(frame: Frame): void {
    switch (frame.t) {
      case 'res': {
        const pending = this.#pending.get(frame.id);
        if (!pending) return;
        this.#pending.delete(frame.id);
        clearTimeout(pending.timer);
        if (frame.error !== undefined) pending.reject(new Error(frame.error));
        else pending.resolve(frame.result ?? null);
        return;
      }
      case 'req':
        this.#onRequest(frame.method, frame.params).then(
          (result) => this.#send({ t: 'res', id: frame.id, result: result ?? null }),
          (error) => this.#send({ t: 'res', id: frame.id, error: errorMessage(error) }),
        );
        return;
      case 'evt':
        try {
          this.#onEvent(frame.name, frame.data);
        } catch (error) {
          console.error(`event ${frame.name} failed:`, error);
        }
    }
  }
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}
