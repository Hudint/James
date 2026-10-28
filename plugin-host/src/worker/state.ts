// Mirror of one instance's state, updated from James' events. Turns the coarse platform
// events into the fine-grained plugin events (nick changes, away, mute, groups, …).

export type Emitted = [name: string, payload: Record<string, unknown>];

/** Platform data as serialised by James (see james-core platform.rs). */
export interface ClientData {
  id: string;
  uid: string;
  name: string;
  channel: string | null;
  serverGroups: string[];
  isSelf: boolean;
  databaseId: string | null;
  channelGroup: string | null;
  description: string;
  phoneticName: string;
  away: string | null;
  inputMuted: boolean;
  outputMuted: boolean;
  recording: boolean;
  talking: boolean;
  country: string;
  query: boolean;
  talkPower: number;
  prioritySpeaker: boolean;
  channelCommander: boolean;
  info: any;
  connection: any;
}

export interface ChannelData {
  id: string;
  parent: string | null;
  name: string;
  [key: string]: unknown;
}

export interface GroupData {
  id: string;
  name: string;
  icon: number;
}

const LEAVE_REASONS: Record<string, string> = {
  disconnect: 'disconnect',
  timeout: 'timeout',
  kicked: 'kicked',
  banned: 'banned',
  serverShutdown: 'serverShutdown',
  invisible: 'invisible',
};

export class InstanceState {
  connected = false;
  server: Record<string, unknown> | null = null;
  ownId: string | null = null;
  clients = new Map<string, ClientData>();
  channels = new Map<string, ChannelData>();
  serverGroups = new Map<string, GroupData>();
  channelGroups = new Map<string, GroupData>();
  /** Last known data of clients/channels that are gone (so wrappers stay readable). */
  gone = new Map<string, ClientData>();
  goneChannels = new Map<string, ChannelData>();
  status: Record<string, any> = {};
  statusAt = Date.now();
  queue: any[] = [];

  constructor(snapshot: any | null, status: any, queue: any[]) {
    if (snapshot) this.load(snapshot);
    this.setStatus(status);
    this.queue = queue ?? [];
  }

  load(snapshot: any): void {
    this.connected = true;
    this.server = snapshot.server ?? null;
    this.ownId = snapshot.ownClient ?? null;
    this.clients = new Map((snapshot.clients ?? []).map((c: ClientData) => [c.id, c]));
    this.channels = new Map((snapshot.channels ?? []).map((c: ChannelData) => [c.id, c]));
    this.serverGroups = new Map((snapshot.serverGroups ?? []).map((g: GroupData) => [g.id, g]));
    this.channelGroups = new Map((snapshot.channelGroups ?? []).map((g: GroupData) => [g.id, g]));
  }

  setStatus(status: any): void {
    this.status = status ?? {};
    this.statusAt = Date.now();
  }

  /** Current position, extrapolated since the last status update. */
  position(): number {
    const s = this.status;
    const base = Number(s.positionMs ?? 0);
    if (!s.current || s.paused || s.loading) return base;
    return base + (Date.now() - this.statusAt);
  }

  client(id: string | null | undefined): ClientData | undefined {
    if (!id) return undefined;
    return this.clients.get(id) ?? this.gone.get(id);
  }

  channel(id: string | null | undefined): ChannelData | undefined {
    if (!id) return undefined;
    return this.channels.get(id) ?? this.goneChannels.get(id);
  }

  /** Applies an instance event; returns the plugin events to emit (`queueChange` is
   * returned as a marker, the runtime fetches the new queue first). */
  apply(envelope: any): Emitted[] {
    switch (envelope.type) {
      case 'connected':
        this.load(envelope.snapshot);
        return [['connect', { server: this.server }]];
      case 'disconnected':
        this.connected = false;
        return [['disconnect', { reason: envelope.reason ?? '' }]];
      case 'platform':
        return this.#platform(envelope.event);
      case 'trackStarted':
        return [['trackStart', { item: envelope.item }]];
      case 'trackEnded':
        return [['trackEnd', { item: envelope.item, error: envelope.error ?? null }]];
      case 'queueChanged':
        return [['queueChange', {}]];
      case 'playbackChanged':
        this.setStatus(envelope.status);
        return [['playbackChange', { status: envelope.status }]];
      default:
        return [];
    }
  }

  #remember(client: ClientData): void {
    // Messages and moves carry the full client; keep the mirror current.
    if (this.clients.has(client.id)) this.clients.set(client.id, client);
  }

  #platform(event: any): Emitted[] {
    switch (event.type) {
      case 'message':
        this.#remember(event.from);
        return [['message', { text: event.text, mode: event.mode, clientId: event.from.id, client: event.from }]];
      case 'poke':
        this.#remember(event.from);
        return [['poke', { text: event.text, clientId: event.from.id, client: event.from }]];
      case 'clientEnter':
        this.clients.set(event.client.id, event.client);
        this.gone.delete(event.client.id);
        return [['clientJoin', { clientId: event.client.id }]];
      case 'clientLeave': {
        const client: ClientData = this.clients.get(event.client.id) ?? event.client;
        this.clients.delete(client.id);
        this.gone.set(client.id, client);
        if (this.gone.size > 1000) this.gone.delete(this.gone.keys().next().value!);
        return [[
          'clientLeave',
          {
            clientId: client.id,
            channelId: client.channel,
            reason: LEAVE_REASONS[event.kind?.kind] ?? 'disconnect',
            banSeconds: event.kind?.seconds ?? null,
            message: event.reason ?? '',
            invokerId: event.invoker?.id ?? null,
            invoker: event.invoker ?? null,
          },
        ]];
      }
      case 'clientMove': {
        const previous = this.clients.get(event.client.id);
        if (event.to === null && event.kind !== 'visible') {
          this.clients.delete(event.client.id);
          this.gone.set(event.client.id, event.client);
        } else {
          this.clients.set(event.client.id, event.client);
          this.gone.delete(event.client.id);
        }
        return [[
          'clientMove',
          {
            clientId: event.client.id,
            fromId: event.from ?? previous?.channel ?? null,
            toId: event.to ?? null,
            invokerId: event.invoker?.id ?? null,
            invoker: event.invoker ?? null,
            reason: event.kind ?? 'switched',
          },
        ]];
      }
      case 'clientUpdate':
        return this.#clientUpdate(event.client);
      case 'channelCreate':
        this.channels.set(event.channel.id, event.channel);
        return [['channelCreate', { channelId: event.channel.id, invoker: event.invoker ?? null }]];
      case 'channelUpdate': {
        const previous = this.channels.get(event.channel.id) ?? event.channel;
        this.channels.set(event.channel.id, event.channel);
        return [['channelUpdate', { channelId: event.channel.id, previous, invoker: event.invoker ?? null }]];
      }
      case 'channelDelete': {
        const channel = this.channels.get(event.id) ?? { id: event.id, parent: null, name: '' };
        this.channels.delete(event.id);
        this.goneChannels.set(event.id, channel);
        return [['channelDelete', { channel, invoker: event.invoker ?? null }]];
      }
      case 'serverUpdate':
        this.server = event.server;
        return [['serverUpdate', { server: event.server }]];
      case 'groupsUpdate':
        this.serverGroups = new Map(event.serverGroups.map((g: GroupData) => [g.id, g]));
        this.channelGroups = new Map(event.channelGroups.map((g: GroupData) => [g.id, g]));
        return [['groupsUpdate', { serverGroups: event.serverGroups, channelGroups: event.channelGroups }]];
      default:
        return [];
    }
  }

  #clientUpdate(client: ClientData): Emitted[] {
    const previous = this.clients.get(client.id);
    this.clients.set(client.id, client);
    if (!previous) return [['clientUpdate', { clientId: client.id, previous: client }]];
    const id = client.id;
    const out: Emitted[] = [['clientUpdate', { clientId: id, previous }]];
    if (previous.name !== client.name) out.push(['clientNick', { clientId: id, oldName: previous.name }]);
    if (previous.away === null && client.away !== null) out.push(['clientAway', { clientId: id, message: client.away }]);
    if (previous.away !== null && client.away === null) out.push(['clientBack', { clientId: id }]);
    if (previous.inputMuted !== client.inputMuted) out.push([client.inputMuted ? 'clientMute' : 'clientUnmute', { clientId: id }]);
    if (previous.outputMuted !== client.outputMuted) out.push([client.outputMuted ? 'clientDeaf' : 'clientUndeaf', { clientId: id }]);
    if (previous.recording !== client.recording) out.push([client.recording ? 'clientRecord' : 'clientRecordStop', { clientId: id }]);
    if (previous.description !== client.description) out.push(['clientDescription', { clientId: id, previous: previous.description }]);
    if (JSON.stringify(previous.info) !== JSON.stringify(client.info) || JSON.stringify(previous.connection) !== JSON.stringify(client.connection)) {
      out.push(['clientInfo', { clientId: id, addressChanged: previous.connection?.address !== client.connection?.address }]);
    }
    for (const group of client.serverGroups) {
      if (!previous.serverGroups.includes(group)) out.push(['serverGroupAdded', { clientId: id, groupId: group }]);
    }
    for (const group of previous.serverGroups) {
      if (!client.serverGroups.includes(group)) out.push(['serverGroupRemoved', { clientId: id, groupId: group }]);
    }
    if (previous.channelGroup !== client.channelGroup) {
      out.push(['channelGroupChanged', { clientId: id, groupId: client.channelGroup, previousId: previous.channelGroup }]);
    }
    return out;
  }
}
