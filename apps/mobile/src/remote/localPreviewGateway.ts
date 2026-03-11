import {
  interruptRemoteTurn,
  listenRemoteThreadRecords,
  listRemoteThreads,
  readRemoteThreadRecord,
  resolveRemoteApproval,
  sendRemotePrompt,
  startRemoteThreadStream,
  stopRemoteThreadStream,
} from "../native/bridge";
import { createRelayBackedGateway } from "./relayGateway";
import type { Thread } from "./protocol";
import { createLocalPreviewRelayConnector } from "./relayConnector";
import type {
  ApprovalResolution,
  DeviceGroup,
  GatewayInspection,
  NativeCapabilities,
  RemoteBootstrap,
  RelayConnectorSnapshot,
  RemoteGateway,
  RemoteSession,
  RemoteThreadRecordEvent,
  RemoteThreadRecord,
  SendTurnInput,
} from "./types";

export function createLocalPreviewGateway(
  snapshot: RelayConnectorSnapshot,
  nativeCapabilities: NativeCapabilities,
): RemoteGateway {
  return createRelayBackedGateway({
    connector: createLocalPreviewRelayConnector(snapshot, nativeCapabilities),
    threadGateway: createLocalPreviewThreadGateway(snapshot, nativeCapabilities),
  });
}

function createLocalPreviewThreadGateway(
  snapshot: RelayConnectorSnapshot,
  nativeCapabilities: NativeCapabilities,
): RemoteGateway {
  let currentSnapshot = snapshot;
  const inspection: GatewayInspection = {
    approvalsResolved: [],
    interrupts: [],
    prompts: [],
  };
  const threadListeners = new Map<
    string,
    Set<(record: RemoteThreadRecord) => void>
  >();
  const activeStreams = new Set<string>();
  const latestRecords = new Map<string, RemoteThreadRecord>();
  let nativeThreadEventsUnlisten: (() => void) | null = null;
  let nativeThreadEventsPromise: Promise<void> | null = null;

  function session(): RemoteSession {
    return {
      ...currentSnapshot.session,
      nativeCapabilities,
    };
  }

  function hosts() {
    return currentSnapshot.hosts;
  }

  function deviceGroups(): DeviceGroup[] {
    return currentSnapshot.deviceGroups;
  }

  function threadKey(hostId: string, threadId: string): string {
    return `${hostId}:${threadId}`;
  }

  async function requireThreadRecord(
    hostId: string,
    threadId: string,
  ): Promise<RemoteThreadRecord> {
    const record = await readRemoteThreadRecord(hostId, threadId);
    if (!record) {
      throw new Error(`Remote preview thread ${hostId}/${threadId} is unavailable.`);
    }
    return record;
  }

  function emitRecord({ hostId, record, threadId }: RemoteThreadRecordEvent) {
    const key = threadKey(hostId, threadId);
    latestRecords.set(key, record);
    for (const listener of threadListeners.get(key) ?? []) {
      listener(record);
    }
  }

  function ensureNativeThreadEvents(): Promise<void> {
    if (nativeThreadEventsUnlisten) {
      return Promise.resolve();
    }
    if (nativeThreadEventsPromise) {
      return nativeThreadEventsPromise;
    }

    nativeThreadEventsPromise = listenRemoteThreadRecords((payload) => {
      emitRecord(payload);
    })
      .then((unlisten) => {
        nativeThreadEventsUnlisten = unlisten;
      })
      .finally(() => {
        nativeThreadEventsPromise = null;
      });

    return nativeThreadEventsPromise;
  }

  async function maybeStopNativeThreadEvents() {
    if (threadListeners.size > 0 || !nativeThreadEventsUnlisten) {
      return;
    }
    const unlisten = nativeThreadEventsUnlisten;
    nativeThreadEventsUnlisten = null;
    await unlisten();
  }

  function subscribeInitialSnapshot(
    hostId: string,
    key: string,
    listener: (record: RemoteThreadRecord) => void,
    threadId: string,
  ) {
    const cached = latestRecords.get(key);
    if (cached) {
      listener(cached);
      return;
    }

    void requireThreadRecord(hostId, threadId)
      .then((record) => {
        const listeners = threadListeners.get(key);
        if (!listeners?.has(listener)) {
          return;
        }
        latestRecords.set(key, record);
        listener(record);
      })
      .catch(() => {});
  }

  function startStream(hostId: string, key: string, threadId: string) {
    if (activeStreams.has(key)) {
      return;
    }

    activeStreams.add(key);
    void ensureNativeThreadEvents()
      .then(() => startRemoteThreadStream(hostId, threadId))
      .catch(() => {
        activeStreams.delete(key);
      });
  }

  function stopStream(hostId: string, key: string, threadId: string) {
    if (!activeStreams.delete(key)) {
      return;
    }
    void stopRemoteThreadStream(hostId, threadId).catch(() => {});
  }

  return {
    async getSession() {
      return session();
    },
    async signIn() {
      currentSnapshot = {
        ...currentSnapshot,
        session: {
          ...currentSnapshot.session,
          signedIn: true,
        },
      };
    },
    async listHosts() {
      return hosts();
    },
    async listThreads(hostId: string): Promise<Thread[]> {
      return (await listRemoteThreads(hostId)) ?? [];
    },
    async getThread(hostId: string, threadId: string) {
      return requireThreadRecord(hostId, threadId);
    },
    async listDeviceGroups() {
      return deviceGroups();
    },
    async resolveApproval(
      hostId: string,
      threadId: string,
      resolution: ApprovalResolution,
    ) {
      inspection.approvalsResolved.push({
        decision: resolution.decision,
        requestId: resolution.requestId,
      });
      await resolveRemoteApproval(hostId, threadId, resolution);
    },
    async interruptTurn(hostId: string, threadId: string) {
      inspection.interrupts.push(threadId);
      await interruptRemoteTurn(hostId, threadId);
    },
    async sendPrompt(hostId: string, threadId: string, input: SendTurnInput) {
      inspection.prompts.push(input.text);
      await sendRemotePrompt(hostId, threadId, input);
    },
    async revokeDevice(hostId: string, deviceId: string) {
      currentSnapshot = {
        ...currentSnapshot,
        deviceGroups: currentSnapshot.deviceGroups.map((group) => {
          if (group.host.id !== hostId) {
            return group;
          }
          return {
            ...group,
            devices: group.devices.map((device) =>
              device.id === deviceId
                ? {
                    ...device,
                    trust: "revoked",
                  }
                : device,
            ),
          };
        }),
      };
    },
    subscribeToThread(hostId: string, threadId: string, listener) {
      const key = threadKey(hostId, threadId);
      const listeners = threadListeners.get(key) ?? new Set();
      listeners.add(listener);
      threadListeners.set(key, listeners);
      subscribeInitialSnapshot(hostId, key, listener, threadId);
      startStream(hostId, key, threadId);

      return () => {
        const current = threadListeners.get(key);
        if (!current) {
          return;
        }
        current.delete(listener);
        if (current.size === 0) {
          threadListeners.delete(key);
          stopStream(hostId, key, threadId);
          void maybeStopNativeThreadEvents();
        }
      };
    },
    subscribeToBootstrap(_listener: (bootstrap: RemoteBootstrap) => void) {
      return () => {};
    },
    inspect() {
      return {
        approvalsResolved: [...inspection.approvalsResolved],
        interrupts: [...inspection.interrupts],
        prompts: [...inspection.prompts],
      };
    },
  };
}
