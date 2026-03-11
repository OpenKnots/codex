import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Thread } from "./protocol";
import type { RelayConnectorSnapshot, RemoteThreadRecord } from "./types";

const bridge = vi.hoisted(() => ({
  interruptRemoteTurn: vi.fn(),
  listenRemoteThreadRecords: vi.fn(),
  listRemoteThreads: vi.fn(),
  readRemoteThreadRecord: vi.fn(),
  resolveRemoteApproval: vi.fn(),
  sendRemotePrompt: vi.fn(),
  startRemoteThreadStream: vi.fn(),
  stopRemoteThreadStream: vi.fn(),
}));

vi.mock("../native/bridge", async () => {
  const actual = await vi.importActual("../native/bridge");
  return {
    ...actual,
    interruptRemoteTurn: bridge.interruptRemoteTurn,
    listenRemoteThreadRecords: bridge.listenRemoteThreadRecords,
    listRemoteThreads: bridge.listRemoteThreads,
    readRemoteThreadRecord: bridge.readRemoteThreadRecord,
    resolveRemoteApproval: bridge.resolveRemoteApproval,
    sendRemotePrompt: bridge.sendRemotePrompt,
    startRemoteThreadStream: bridge.startRemoteThreadStream,
    stopRemoteThreadStream: bridge.stopRemoteThreadStream,
  };
});

import { createLocalPreviewGateway } from "./localPreviewGateway";

describe("createLocalPreviewGateway", () => {
  beforeEach(() => {
    bridge.interruptRemoteTurn.mockReset();
    bridge.listenRemoteThreadRecords.mockReset();
    bridge.listRemoteThreads.mockReset();
    bridge.readRemoteThreadRecord.mockReset();
    bridge.resolveRemoteApproval.mockReset();
    bridge.sendRemotePrompt.mockReset();
    bridge.startRemoteThreadStream.mockReset();
    bridge.stopRemoteThreadStream.mockReset();
    bridge.listenRemoteThreadRecords.mockResolvedValue(() => {});
    bridge.startRemoteThreadStream.mockResolvedValue(undefined);
    bridge.stopRemoteThreadStream.mockResolvedValue(undefined);
    bridge.resolveRemoteApproval.mockResolvedValue(undefined);
  });

  it("reads thread listings and records from the native local preview bridge", async () => {
    const gateway = createLocalPreviewGateway(
      createSnapshot(),
      {
        fileImport: true,
        qrScanner: true,
        relaySockets: true,
        secureStore: true,
      },
    );
    const thread = createThread({
      id: "thread-live-ios",
      name: "Ship the iOS remote shell",
      preview: "Use the UDS preview bridge for live history.",
    });
    const record = createRecord(thread);
    bridge.listRemoteThreads.mockResolvedValue([thread]);
    bridge.readRemoteThreadRecord.mockResolvedValue(record);

    await expect(gateway.listThreads("host-local-preview")).resolves.toEqual([
      thread,
    ]);
    await expect(
      gateway.getThread("host-local-preview", "thread-live-ios"),
    ).resolves.toEqual(record);
  });

  it("routes send and interrupt actions through the native local preview bridge", async () => {
    const gateway = createLocalPreviewGateway(
      createSnapshot(),
      {
        fileImport: true,
        qrScanner: true,
        relaySockets: true,
        secureStore: true,
      },
    );
    bridge.listRemoteThreads.mockResolvedValue([]);
    bridge.readRemoteThreadRecord.mockResolvedValue(
      createRecord(
        createThread({
          id: "thread-live-ios",
          name: "Ship the iOS remote shell",
          preview: "Use the UDS preview bridge for live history.",
        }),
      ),
    );

    await gateway.sendPrompt("host-local-preview", "thread-live-ios", {
      mode: "newTurn",
      text: "Continue with the relay adapter.",
    });
    await gateway.interruptTurn("host-local-preview", "thread-live-ios");

    expect(bridge.sendRemotePrompt).toHaveBeenCalledWith(
      "host-local-preview",
      "thread-live-ios",
      {
        mode: "newTurn",
        text: "Continue with the relay adapter.",
      },
    );
    expect(bridge.interruptRemoteTurn).toHaveBeenCalledWith(
      "host-local-preview",
      "thread-live-ios",
    );
  });

  it("starts one native stream per subscribed thread and stops it after the last listener leaves", async () => {
    const gateway = createLocalPreviewGateway(
      createSnapshot(),
      {
        fileImport: true,
        qrScanner: true,
        relaySockets: true,
        secureStore: true,
      },
    );
    const record = createRecord(
      createThread({
        id: "thread-live-ios",
        name: "Ship the iOS remote shell",
        preview: "Use the UDS preview bridge for live history.",
      }),
    );
    bridge.readRemoteThreadRecord.mockResolvedValue(record);

    const listenerOne = vi.fn();
    const listenerTwo = vi.fn();

    const unsubscribeOne = gateway.subscribeToThread(
      "host-local-preview",
      "thread-live-ios",
      listenerOne,
    );
    const unsubscribeTwo = gateway.subscribeToThread(
      "host-local-preview",
      "thread-live-ios",
      listenerTwo,
    );

    await vi.waitFor(() => {
      expect(bridge.listenRemoteThreadRecords).toHaveBeenCalledTimes(1);
      expect(bridge.startRemoteThreadStream).toHaveBeenCalledWith(
        "host-local-preview",
        "thread-live-ios",
      );
    });

    unsubscribeOne();
    expect(bridge.stopRemoteThreadStream).not.toHaveBeenCalled();

    unsubscribeTwo();

    await vi.waitFor(() => {
      expect(bridge.stopRemoteThreadStream).toHaveBeenCalledWith(
        "host-local-preview",
        "thread-live-ios",
      );
    });
  });

  it("routes native pushed thread records to local subscribers", async () => {
    let emitRecord:
      | ((payload: {
          hostId: string;
          record: RemoteThreadRecord;
          threadId: string;
        }) => void)
      | undefined;
    bridge.listenRemoteThreadRecords.mockImplementation(
      async (
        listener: (payload: {
          hostId: string;
          record: RemoteThreadRecord;
          threadId: string;
        }) => void,
      ) => {
        emitRecord = listener;
        return () => {};
      },
    );
    const gateway = createLocalPreviewGateway(
      createSnapshot(),
      {
        fileImport: true,
        qrScanner: true,
        relaySockets: true,
        secureStore: true,
      },
    );
    const initialRecord = createRecord(
      createThread({
        id: "thread-live-ios",
        name: "Ship the iOS remote shell",
        preview: "Use the UDS preview bridge for live history.",
      }),
    );
    const pushedRecord: RemoteThreadRecord = {
      ...initialRecord,
      runtime: {
        ...initialRecord.runtime,
        phase: "running",
        statusCopy: "Turn is active on the host.",
      },
      thread: {
        ...initialRecord.thread,
        preview: "Streaming live updates from the host daemon.",
      },
    };
    bridge.readRemoteThreadRecord.mockResolvedValue(initialRecord);
    const listener = vi.fn();

    const unsubscribe = gateway.subscribeToThread(
      "host-local-preview",
      "thread-live-ios",
      listener,
    );

    await vi.waitFor(() => {
      expect(listener).toHaveBeenCalledWith(initialRecord);
      expect(emitRecord).toBeTypeOf("function");
    });

    emitRecord?.({
      hostId: "host-local-preview",
      record: pushedRecord,
      threadId: "thread-live-ios",
    });

    await vi.waitFor(() => {
      expect(listener).toHaveBeenLastCalledWith(pushedRecord);
    });

    unsubscribe();
  });

  it("routes approval resolution through the native local preview bridge", async () => {
    const gateway = createLocalPreviewGateway(
      createSnapshot(),
      {
        fileImport: true,
        qrScanner: true,
        relaySockets: true,
        secureStore: true,
      },
    );

    await gateway.resolveApproval("host-local-preview", "thread-live-ios", {
      decision: "acceptForSession",
      requestId: "approval-42",
    });

    expect(bridge.resolveRemoteApproval).toHaveBeenCalledWith(
      "host-local-preview",
      "thread-live-ios",
      {
        decision: "acceptForSession",
        requestId: "approval-42",
      },
    );
    expect(gateway.inspect().approvalsResolved).toEqual([
      {
        decision: "acceptForSession",
        requestId: "approval-42",
      },
    ]);
  });
});

function createSnapshot(): RelayConnectorSnapshot {
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
          relayStatus: "localPreview",
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
        relayStatus: "localPreview",
        status: "online",
      },
    ],
    session: {
      accountLabel: "Local preview",
      pairingCode: "PAIR-LOCAL",
      pairingUrl: "codex://remote/pair?code=PAIR-LOCAL",
      signedIn: true,
      workspaceLabel: "Val Dev Mac",
    },
  };
}

function createThread({
  id,
  name,
  preview,
}: {
  id: string;
  name: string;
  preview: string;
}): Thread {
  return {
    agentNickname: null,
    agentRole: null,
    cliVersion: "1.0.0",
    createdAt: 1720000000,
    cwd: "/workspace/codex",
    ephemeral: false,
    gitInfo: null,
    id,
    modelProvider: "openai",
    name,
    path: null,
    preview,
    source: "appServer",
    status: {
      activeFlags: [],
      type: "active",
    },
    turns: [],
    updatedAt: 1720000300,
  };
}

function createRecord(thread: Thread): RemoteThreadRecord {
  return {
    approvals: [],
    hostId: "host-local-preview",
    runtime: {
      composerMode: "newTurn",
      connection: "online",
      phase: "completed",
      statusCopy: "No active turn on this thread.",
    },
    thread,
  };
}
