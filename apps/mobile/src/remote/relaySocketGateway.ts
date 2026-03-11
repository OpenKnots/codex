import type { Thread } from "./protocol";
import type { RelaySocketClient, RelaySocketClientEvent } from "./relaySocketClient";
import type {
  ApprovalResolution,
  GatewayInspection,
  NativeCapabilities,
  RemoteBootstrap,
  RemoteGateway,
  RemoteThreadRecord,
  SendTurnInput,
} from "./types";

export function createRelaySocketGateway({
  client,
  nativeCapabilities,
}: {
  client: RelaySocketClient;
  nativeCapabilities?: NativeCapabilities;
}): RemoteGateway {
  const bootstrapListeners = new Set<(bootstrap: RemoteBootstrap) => void>();
  const threadListeners = new Map<string, Set<(record: RemoteThreadRecord) => void>>();
  const threadRecords = new Map<string, RemoteThreadRecord>();
  const threadSubscriptions = new Map<string, { hostId: string; threadId: string }>();
  const inspection: GatewayInspection = {
    approvalsResolved: [],
    interrupts: [],
    prompts: [],
  };
  let bootstrapPromise: Promise<RemoteBootstrap> | null = null;
  let currentBootstrap: RemoteBootstrap | null = null;
  let stableBootstrap: RemoteBootstrap | null = null;

  client.subscribe((event: RelaySocketClientEvent) => {
    if (event.type === "connection/status") {
      if (event.status === "reconnecting") {
        if (!stableBootstrap) {
          return;
        }
        currentBootstrap = {
          ...stableBootstrap,
          hosts: stableBootstrap.hosts.map((host) => ({
            ...host,
            relayStatus: "Relay reconnecting",
          })),
        };
        emitBootstrap();
        return;
      }
      if (stableBootstrap) {
        currentBootstrap = stableBootstrap;
        emitBootstrap();
      }
      resubscribeThreads();
      return;
    }
    if (event.type === "bootstrap/update") {
      currentBootstrap = normalizeBootstrap(event.bootstrap, nativeCapabilities);
      stableBootstrap = currentBootstrap;
      emitBootstrap();
      return;
    }

    const key = threadKey(event.hostId, event.threadId);
    threadRecords.set(key, event.record);
    emitThread(key, event.record);
  });

  function normalizeBootstrap(
    bootstrap: RemoteBootstrap,
    localCapabilities?: NativeCapabilities,
  ): RemoteBootstrap {
    if (!localCapabilities) {
      return bootstrap;
    }
    return {
      ...bootstrap,
      session: {
        ...bootstrap.session,
        nativeCapabilities: localCapabilities,
      },
    };
  }

  async function getBootstrap(): Promise<RemoteBootstrap> {
    if (currentBootstrap) {
      return currentBootstrap;
    }
    if (!bootstrapPromise) {
      bootstrapPromise = client
        .request<RemoteBootstrap>("bootstrap/get")
        .then((bootstrap) => {
          currentBootstrap = normalizeBootstrap(bootstrap, nativeCapabilities);
          stableBootstrap = currentBootstrap;
          return currentBootstrap;
        })
        .finally(() => {
          bootstrapPromise = null;
        });
    }
    return bootstrapPromise;
  }

  function emitBootstrap() {
    if (!currentBootstrap) {
      return;
    }
    for (const listener of bootstrapListeners) {
      listener(currentBootstrap);
    }
  }

  function emitThread(key: string, record: RemoteThreadRecord) {
    for (const listener of threadListeners.get(key) ?? []) {
      listener(record);
    }
  }

  async function readThreadRecord(
    hostId: string,
    threadId: string,
  ): Promise<RemoteThreadRecord> {
    const key = threadKey(hostId, threadId);
    const cached = threadRecords.get(key);
    if (cached) {
      return cached;
    }
    const record = await client.request<RemoteThreadRecord>("thread/read", {
      hostId,
      threadId,
    });
    threadRecords.set(key, record);
    return record;
  }

  function subscribeThread(hostId: string, threadId: string) {
    const key = threadKey(hostId, threadId);
    if (threadSubscriptions.has(key)) {
      return;
    }
    threadSubscriptions.set(key, { hostId, threadId });
    void client.request("thread/subscribe", {
      hostId,
      threadId,
    });
  }

  function unsubscribeThread(hostId: string, threadId: string) {
    const key = threadKey(hostId, threadId);
    if (!threadSubscriptions.delete(key)) {
      return;
    }
    void client.request("thread/unsubscribe", {
      hostId,
      threadId,
    });
  }

  function resubscribeThreads() {
    for (const [key, subscription] of threadSubscriptions) {
      void client.request("thread/subscribe", subscription);
      void client
        .request<RemoteThreadRecord>("thread/read", subscription)
        .then((record) => {
          threadRecords.set(key, record);
          emitThread(key, record);
        })
        .catch(() => {});
    }
  }

  return {
    async getSession() {
      return (await getBootstrap()).session;
    },
    async signIn() {
      const bootstrap = await client.request<RemoteBootstrap>("session/signIn");
      currentBootstrap = normalizeBootstrap(bootstrap, nativeCapabilities);
      stableBootstrap = currentBootstrap;
      emitBootstrap();
    },
    async listHosts() {
      return (await getBootstrap()).hosts;
    },
    async listThreads(hostId: string): Promise<Thread[]> {
      return client.request<Thread[]>("thread/list", {
        hostId,
      });
    },
    async getThread(hostId: string, threadId: string) {
      return readThreadRecord(hostId, threadId);
    },
    async listDeviceGroups() {
      return (await getBootstrap()).deviceGroups;
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
      await client.request("approval/resolve", {
        hostId,
        resolution,
        threadId,
      });
    },
    async interruptTurn(hostId: string, threadId: string) {
      inspection.interrupts.push(threadId);
      await client.request("turn/interrupt", {
        hostId,
        threadId,
      });
    },
    async sendPrompt(hostId: string, threadId: string, input: SendTurnInput) {
      inspection.prompts.push(input.text);
      await client.request("turn/prompt", {
        hostId,
        input,
        threadId,
      });
    },
    async revokeDevice(hostId: string, deviceId: string) {
      const bootstrap = await client.request<RemoteBootstrap>("device/revoke", {
        deviceId,
        hostId,
      });
      currentBootstrap = normalizeBootstrap(bootstrap, nativeCapabilities);
      stableBootstrap = currentBootstrap;
      emitBootstrap();
    },
    subscribeToThread(hostId: string, threadId: string, listener) {
      const key = threadKey(hostId, threadId);
      const listeners = threadListeners.get(key) ?? new Set();
      listeners.add(listener);
      threadListeners.set(key, listeners);
      subscribeThread(hostId, threadId);

      const cached = threadRecords.get(key);
      if (cached) {
        listener(cached);
      } else {
        void readThreadRecord(hostId, threadId).then((record) => {
          if (threadListeners.get(key)?.has(listener)) {
            listener(record);
          }
        });
      }

      return () => {
        const current = threadListeners.get(key);
        if (!current) {
          return;
        }
        current.delete(listener);
        if (current.size === 0) {
          threadListeners.delete(key);
          unsubscribeThread(hostId, threadId);
        }
      };
    },
    subscribeToBootstrap(listener) {
      bootstrapListeners.add(listener);
      if (currentBootstrap) {
        listener(currentBootstrap);
      } else {
        void getBootstrap().then((bootstrap) => {
          if (bootstrapListeners.has(listener)) {
            listener(bootstrap);
          }
        });
      }
      return () => {
        bootstrapListeners.delete(listener);
      };
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

function threadKey(hostId: string, threadId: string): string {
  return `${hostId}:${threadId}`;
}
