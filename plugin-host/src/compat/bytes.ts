// SinusBot `Bytes` (reader) and `BytesWriter`.

export class Bytes {
  readonly #buf: Buffer;
  #pos = 0;

  constructor(buf: Buffer) {
    this.#buf = buf;
  }
  static of(value: unknown): Bytes {
    if (value instanceof Bytes) return value;
    if (value instanceof BytesWriter) return value.newReader();
    return new Bytes(toBuffer(value));
  }
  buffer(): Buffer {
    return this.#buf;
  }
  bytes(): number[] {
    return [...this.#buf];
  }
  string(length: number): string {
    const s = this.#buf.subarray(this.#pos, this.#pos + length).toString('utf8');
    this.#pos = Math.min(this.#buf.length, this.#pos + length);
    return s;
  }
  cString(): string {
    const end = this.#buf.indexOf(0, this.#pos);
    const stop = end < 0 ? this.#buf.length : end;
    const s = this.#buf.subarray(this.#pos, stop).toString('utf8');
    this.#pos = Math.min(this.#buf.length, stop + 1);
    return s;
  }
  #read(size: number, fn: (offset: number) => number): number {
    if (this.#pos + size > this.#buf.length) throw new RangeError('read past the end of the buffer');
    const v = fn(this.#pos);
    this.#pos += size;
    return v;
  }
  uint8() { return this.#read(1, (o) => this.#buf.readUInt8(o)); }
  int16be() { return this.#read(2, (o) => this.#buf.readInt16BE(o)); }
  int16le() { return this.#read(2, (o) => this.#buf.readInt16LE(o)); }
  uint16be() { return this.#read(2, (o) => this.#buf.readUInt16BE(o)); }
  uint16le() { return this.#read(2, (o) => this.#buf.readUInt16LE(o)); }
  int32be() { return this.#read(4, (o) => this.#buf.readInt32BE(o)); }
  int32le() { return this.#read(4, (o) => this.#buf.readInt32LE(o)); }
  uint32be() { return this.#read(4, (o) => this.#buf.readUInt32BE(o)); }
  uint32le() { return this.#read(4, (o) => this.#buf.readUInt32LE(o)); }
  length() { return this.#buf.length; }
  pos() { return this.#pos; }
  seek(pos: number) {
    if (pos < 0 || pos > this.#buf.length) return false;
    this.#pos = pos;
    return true;
  }
  toBase64() { return this.#buf.toString('base64'); }
  toHex() { return this.#buf.toString('hex'); }
  toString() { return this.#buf.toString('utf8'); }
  toJSON() { return this.toString(); }
}

export class BytesWriter {
  #parts: Buffer[] = [];

  constructor(initial?: Buffer) {
    if (initial) this.#parts.push(initial);
  }
  #buffer(): Buffer {
    if (this.#parts.length > 1) this.#parts = [Buffer.concat(this.#parts)];
    return this.#parts[0] ?? Buffer.alloc(0);
  }
  buffer(): Buffer {
    return this.#buffer();
  }
  append(bytes: Bytes | BytesWriter | unknown): number {
    const b = bytes instanceof Bytes ? bytes.buffer() : bytes instanceof BytesWriter ? bytes.buffer() : toBuffer(bytes);
    this.#parts.push(b);
    return b.length;
  }
  bytes(): Bytes { return new Bytes(this.#buffer()); }
  newReader(): Bytes { return new Bytes(this.#buffer()); }
  length(): number { return this.#buffer().length; }
  toBase64() { return this.#buffer().toString('base64'); }
  toHex() { return this.#buffer().toString('hex'); }
  toString() { return this.#buffer().toString('utf8'); }
  writeString(s: string) { this.#parts.push(Buffer.from(String(s), 'utf8')); }
  writeCString(s: string) { this.#parts.push(Buffer.from(String(s) + '\0', 'utf8')); }
  #write(size: number, fn: (b: Buffer) => void) {
    const b = Buffer.alloc(size);
    fn(b);
    this.#parts.push(b);
  }
  writeInt8(v: number) { this.#write(1, (b) => b.writeInt8(v)); }
  writeUint8(v: number) { this.#write(1, (b) => b.writeUInt8(v)); }
  writeInt16be(v: number) { this.#write(2, (b) => b.writeInt16BE(v)); }
  writeInt16le(v: number) { this.#write(2, (b) => b.writeInt16LE(v)); }
  writeUint16be(v: number) { this.#write(2, (b) => b.writeUInt16BE(v)); }
  writeUint16le(v: number) { this.#write(2, (b) => b.writeUInt16LE(v)); }
  writeInt32be(v: number) { this.#write(4, (b) => b.writeInt32BE(v)); }
  writeInt32le(v: number) { this.#write(4, (b) => b.writeInt32LE(v)); }
  writeUint32be(v: number) { this.#write(4, (b) => b.writeUInt32BE(v)); }
  writeUint32le(v: number) { this.#write(4, (b) => b.writeUInt32LE(v)); }
  writeInt64be(v: number) { this.#write(8, (b) => b.writeBigInt64BE(BigInt(v))); }
  writeInt64le(v: number) { this.#write(8, (b) => b.writeBigInt64LE(BigInt(v))); }
  writeUint64be(v: number) { this.#write(8, (b) => b.writeBigUInt64BE(BigInt(v))); }
  writeUint64le(v: number) { this.#write(8, (b) => b.writeBigUInt64LE(BigInt(v))); }
}

export function toBuffer(value: unknown): Buffer {
  if (value instanceof Bytes || value instanceof BytesWriter) return value.buffer();
  if (Buffer.isBuffer(value)) return value;
  if (value instanceof Uint8Array) return Buffer.from(value);
  if (Array.isArray(value)) return Buffer.from(value);
  if (value === undefined || value === null) return Buffer.alloc(0);
  return Buffer.from(String(value), 'utf8');
}
