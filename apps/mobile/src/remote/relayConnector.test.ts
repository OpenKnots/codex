import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createMockGateway } from "./mockGateway";
import {
  createGatewayBackedRelayConnector,
  createLocalPreviewRelayConnector,
} from "./relayConnector";
import type {
  NativeCapabilities,
  RelayConnectorSnapshot,
} from "./types";

const bridge = vi.hoisted(() => ({
  readRemoteConnectorSnapshot: vi.fn(),
}));

vi.mock("../native/bridge", async () => {
  const actual = await vi.importActual("../native/bridge");
  return {
    ...actual,
    readRemoteConnectorSnapshot: bridge.readRemoteConnectorSnapshot,
  };
});

describe("relay connectors", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    bridge.readRemoteConnectorSnapshot.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("publishes updated local preview bootstrap snapshots", async () => {
    const connector = createLocalPreviewRelayConnector(
      createSnapshot({
        session: {
          pairingCode: "PAIR-LOCAL",
        },
      }),
      createNativeCapabilities(),
    );
    const listener = vi.fn();
    bridge.readRemoteConnectorSnapshot.mockResolvedValue(
      createSnapshot({
        hosts: [
          {
            relayStatus: "relayConnected",
          },
        ],
        session: {
          pairingCode: "PAIR-UPDATED",
        },
      }),
    );

    const unsubscribe = connector.subscribe(listener);

    await vi.waitFor(() => {
      expect(listener).toHaveBeenCalledWith(
        expect.objectContaining({
          hosts: [expect.objectContaining({ relayStatus: "localPreview" })],
          session: expect.objectContaining({ pairingCode: "PAIR-LOCAL" }),
        }),
      );
    });

    await vi.advanceTimersByTimeAsync(2_100);

    await vi.waitFor(() => {
      expect(listener).toHaveBeenLastCalledWith(
        expect.objectContaining({
          hosts: [expect.objectContaining({ relayStatus: "relayConnected" })],
          session: expect.objectContaining({ pairingCode: "PAIR-UPDATED" }),
        }),
      );
    });

    unsubscribe();
  });

  it("keeps optimistic device revocations when local preview snapshots refresh", async () => {
    const connector = createLocalPreviewRelayConnector(
      createSnapshot(),
      createNativeCapabilities(),
    );
    const listener = vi.fn();
    bridge.readRemoteConnectorSnapshot.mockResolvedValue(createSnapshot());

    const unsubscribe = connector.subscribe(listener);

    await connector.revokeDevice("host-local-preview", "device-local");
    await vi.advanceTimersByTimeAsync(2_100);

    await vi.waitFor(() => {
      expect(listener).toHaveBeenLastCalledWith(
        expect.objectContaining({
          deviceGroups: [
            expect.objectContaining({
              devices: [expect.objectContaining({ trust: "revoked" })],
            }),
          ],
        }),
      );
    });

    unsubscribe();
  });

  it("publishes bootstrap updates from gateway-backed connectors after local actions", async () => {
    const connector = createGatewayBackedRelayConnector(createMockGateway());
    const listener = vi.fn();

    const unsubscribe = connector.subscribe(listener);

    await vi.waitFor(() => {
      expect(listener.mock.calls[0]?.[0].deviceGroups[0]?.devices[0]?.trust).toBe(
        "trusted",
      );
    });

    await connector.revokeDevice("host-studio", "device-iphone-main");

    await vi.waitFor(() => {
      expect(
        listener.mock.calls.at(-1)?.[0].deviceGroups[0]?.devices[0]?.trust,
      ).toBe("revoked");
      expect(
        listener.mock.calls.at(-1)?.[0].deviceGroups[0]?.host.id,
      ).toBe("host-studio");
    });

    unsubscribe();
  });
});

function createSnapshot(overrides?: {
  hosts?: Array<Partial<RelayConnectorSnapshot["hosts"][number]>>;
  session?: Partial<RelayConnectorSnapshot["session"]>;
}): RelayConnectorSnapshot {
  return {
    connectorMode: "localPreview",
    deviceGroups: [
      {
        devices: [
          {
            id: "device-local",
            lastSeenAt: 1720000200,
            name: "Preview iPhone",
            pairedAt: 1720000000,
            transport: "Local preview",
            trust: "trusted",
          },
        ],
        host: {
          detail: "Host runtime ready for preview",
          id: "host-local-preview",
          lastSeenAt: 1720000300,
          name: "Val Dev Mac",
          pairedAt: 1720000000,
          platform: "macOS",
          relayStatus: overrides?.hosts?.[0]?.relayStatus ?? "localPreview",
          status: "online",
        },
      },
    ],
    hosts: [
      {
        detail: "Host runtime ready for preview",
        id: "host-local-preview",
        lastSeenAt: 1720000300,
        name: "Val Dev Mac",
        pairedAt: 1720000000,
        platform: "macOS",
        relayStatus: overrides?.hosts?.[0]?.relayStatus ?? "localPreview",
        status: "online",
      },
    ],
    session: {
      accountLabel: "Local preview",
      pairingCode: overrides?.session?.pairingCode ?? "PAIR-LOCAL",
      pairingUrl: "codex://remote/pair?code=PAIR-LOCAL",
      signedIn: true,
      workspaceLabel: "Val Dev Mac",
    },
  };
}

function createNativeCapabilities(): NativeCapabilities {
  return {
    fileImport: true,
    qrScanner: true,
    relaySockets: true,
    secureStore: true,
  };
}
