import {
  listenRemoteConnectorSnapshots,
  readRemoteConnectorSnapshot,
  startRemoteConnectorStream,
  stopRemoteConnectorStream,
} from "../native/bridge";
import type {
  DeviceGroup,
  NativeCapabilities,
  RelayConnectorSnapshot,
  RemoteBootstrap,
  RemoteGateway,
} from "./types";

const CONNECTOR_POLL_INTERVAL_MS = 2_000;

export interface RelayConnector {
  getBootstrap(): Promise<RemoteBootstrap>;
  revokeDevice(hostId: string, deviceId: string): Promise<void>;
  signIn(): Promise<void>;
  subscribe(listener: (bootstrap: RemoteBootstrap) => void): () => void;
}

export function createGatewayBackedRelayConnector(
  gateway: Pick<
    RemoteGateway,
    "getSession" | "listDeviceGroups" | "listHosts" | "revokeDevice" | "signIn"
  >,
): RelayConnector {
  const listeners = new Set<(bootstrap: RemoteBootstrap) => void>();

  async function readBootstrap(): Promise<RemoteBootstrap> {
    const [session, hosts, deviceGroups] = await Promise.all([
      gateway.getSession(),
      gateway.listHosts(),
      gateway.listDeviceGroups(),
    ]);
    return {
      deviceGroups,
      hosts,
      session,
    };
  }

  async function emitBootstrap() {
    const bootstrap = await readBootstrap();
    for (const listener of listeners) {
      listener(bootstrap);
    }
  }

  return {
    getBootstrap() {
      return readBootstrap();
    },
    async revokeDevice(hostId, deviceId) {
      await gateway.revokeDevice(hostId, deviceId);
      await emitBootstrap();
    },
    async signIn() {
      await gateway.signIn();
      await emitBootstrap();
    },
    subscribe(listener) {
      listeners.add(listener);
      void readBootstrap().then(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}

export function createLocalPreviewRelayConnector(
  snapshot: RelayConnectorSnapshot,
  nativeCapabilities: NativeCapabilities,
): RelayConnector {
  let currentSnapshot = snapshot;
  let signedInOverride = false;
  const trustOverrides = new Map<string, "trusted" | "revoked">();
  const listeners = new Set<(bootstrap: RemoteBootstrap) => void>();
  let nativeUnsubscribe: (() => void) | undefined;
  let pollTimer: ReturnType<typeof setTimeout> | undefined;
  let subscriptionMode: "idle" | "native" | "polling" = "idle";
  let polling = false;

  function overlaySnapshot(nextSnapshot: RelayConnectorSnapshot) {
    const currentDevices = nextSnapshot.deviceGroups.map((group) => ({
      ...group,
      devices: group.devices.map((device) => ({
        ...device,
        trust: trustOverrides.get(device.id) ?? device.trust,
      })),
    }));
    currentSnapshot = {
      ...nextSnapshot,
      deviceGroups: currentDevices,
      hosts: nextSnapshot.hosts.map((host) => ({
        ...host,
      })),
      session: {
        ...nextSnapshot.session,
        signedIn: nextSnapshot.session.signedIn || signedInOverride,
      },
    };
  }

  function applySnapshot(nextSnapshot: RelayConnectorSnapshot) {
    const previousSnapshot = JSON.stringify(currentSnapshot);
    overlaySnapshot(nextSnapshot);
    if (JSON.stringify(currentSnapshot) !== previousSnapshot) {
      emitBootstrap();
    }
  }

  function currentBootstrap(): RemoteBootstrap {
    return {
      deviceGroups: currentSnapshot.deviceGroups,
      hosts: currentSnapshot.hosts,
      session: {
        ...currentSnapshot.session,
        nativeCapabilities,
      },
    };
  }

  function emitBootstrap() {
    const bootstrap = currentBootstrap();
    for (const listener of listeners) {
      listener(bootstrap);
    }
  }

  async function pollSnapshot() {
    if (!polling) {
      return;
    }

    const nextSnapshot = await readRemoteConnectorSnapshot();
    if (nextSnapshot) {
      applySnapshot(nextSnapshot);
    }

    if (polling) {
      pollTimer = setTimeout(() => {
        void pollSnapshot();
      }, CONNECTOR_POLL_INTERVAL_MS);
    }
  }

  function startPolling() {
    if (polling) {
      return;
    }
    subscriptionMode = "polling";
    polling = true;
    void pollSnapshot();
  }

  function stopPolling() {
    if (!polling) {
      return;
    }
    polling = false;
    if (pollTimer) {
      clearTimeout(pollTimer);
      pollTimer = undefined;
    }
    if (subscriptionMode === "polling") {
      subscriptionMode = "idle";
    }
  }

  async function startNativeStreaming() {
    if (subscriptionMode !== "idle" || listeners.size === 0) {
      return;
    }

    try {
      const unsubscribe = await listenRemoteConnectorSnapshots(applySnapshot);
      if (listeners.size === 0) {
        unsubscribe();
        return;
      }
      await startRemoteConnectorStream();
      if (listeners.size === 0) {
        unsubscribe();
        void stopRemoteConnectorStream();
        return;
      }
      nativeUnsubscribe = unsubscribe;
      subscriptionMode = "native";
    } catch {
      nativeUnsubscribe?.();
      nativeUnsubscribe = undefined;
      startPolling();
    }
  }

  function stopNativeStreaming() {
    if (subscriptionMode !== "native") {
      return;
    }
    nativeUnsubscribe?.();
    nativeUnsubscribe = undefined;
    subscriptionMode = "idle";
    void stopRemoteConnectorStream();
  }

  return {
    async getBootstrap() {
      return currentBootstrap();
    },
    async revokeDevice(hostId, deviceId) {
      trustOverrides.set(deviceId, "revoked");
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
      emitBootstrap();
    },
    async signIn() {
      signedInOverride = true;
      currentSnapshot = {
        ...currentSnapshot,
        session: {
          ...currentSnapshot.session,
          signedIn: true,
        },
      };
      emitBootstrap();
    },
    subscribe(listener) {
      listeners.add(listener);
      listener(currentBootstrap());
      if (listeners.size === 1) {
        void startNativeStreaming();
      }
      return () => {
        listeners.delete(listener);
        if (listeners.size === 0) {
          stopNativeStreaming();
          stopPolling();
        }
      };
    },
  };
}
