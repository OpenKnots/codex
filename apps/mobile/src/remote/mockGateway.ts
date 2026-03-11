import type {
  AdditionalPermissionProfile,
  CommandExecutionRequestApprovalParams,
  Thread,
  ThreadItem,
  UserInput,
} from "./protocol";
import type {
  ApprovalResolution,
  DeviceGroup,
  GatewayInspection,
  HostSummary,
  NativeCapabilities,
  PairedDevice,
  RemoteApproval,
  RemoteGateway,
  RemoteSession,
  RemoteThreadRecord,
  SendTurnInput,
} from "./types";

type MockGatewayOptions = {
  signedIn?: boolean;
  nativeCapabilities?: NativeCapabilities;
};

type MutableState = {
  session: RemoteSession;
  hosts: HostSummary[];
  deviceGroups: DeviceGroup[];
  threadRecords: Record<string, Record<string, RemoteThreadRecord>>;
  approvalsResolved: GatewayInspection["approvalsResolved"];
  interrupts: string[];
  prompts: string[];
};

export function createMockGateway(
  options: MockGatewayOptions = {},
): RemoteGateway {
  const listeners = new Map<
    string,
    Set<(record: RemoteThreadRecord) => void>
  >();
  const state = createState(options);

  function getRecord(hostId: string, threadId: string): RemoteThreadRecord {
    const hostRecords = state.threadRecords[hostId];
    if (!hostRecords || !hostRecords[threadId]) {
      throw new Error(`Unknown mock thread ${hostId}/${threadId}`);
    }
    return hostRecords[threadId];
  }

  function emit(hostId: string, threadId: string) {
    const key = listenerKey(hostId, threadId);
    const record = clone(getRecord(hostId, threadId));
    for (const listener of listeners.get(key) ?? []) {
      listener(record);
    }
  }

  return {
    async getSession() {
      return clone(state.session);
    },
    async signIn() {
      state.session.signedIn = true;
    },
    async listHosts() {
      return state.session.signedIn ? clone(state.hosts) : [];
    },
    async listThreads(hostId: string) {
      if (!state.session.signedIn) {
        return [];
      }

      return Object.values(state.threadRecords[hostId] ?? {}).map((record) => ({
        ...clone(record.thread),
        turns: [],
      }));
    },
    async getThread(hostId: string, threadId: string) {
      return clone(getRecord(hostId, threadId));
    },
    async listDeviceGroups() {
      return state.session.signedIn ? clone(state.deviceGroups) : [];
    },
    async resolveApproval(
      hostId: string,
      threadId: string,
      resolution: ApprovalResolution,
    ) {
      const record = getRecord(hostId, threadId);
      state.approvalsResolved.push({
        decision: resolution.decision,
        requestId: resolution.requestId,
      });
      record.approvals = record.approvals.filter(
        (approval) => approval.requestId !== resolution.requestId,
      );
      record.runtime.phase =
        record.approvals.length > 0 ? "waitingOnApproval" : "running";
      record.runtime.statusCopy =
        resolution.decision === "acceptForSession"
          ? "Session approval granted from iPhone."
          : "Turn approval granted from iPhone.";
      record.thread.status =
        record.approvals.length > 0
          ? { type: "active", activeFlags: ["waitingOnApproval"] }
          : { type: "active", activeFlags: [] };
      appendAgentMessage(
        record.thread,
        resolution.decision === "acceptForSession"
          ? "Approved this command for the active session from the phone."
          : "Approved this command for the active turn from the phone.",
      );
      emit(hostId, threadId);
    },
    async interruptTurn(hostId: string, threadId: string) {
      const record = getRecord(hostId, threadId);
      state.interrupts.push(threadId);
      record.runtime.phase = "completed";
      record.runtime.statusCopy = "Turn interrupted from iPhone.";
      record.thread.status = { type: "idle" };
      appendAgentMessage(
        record.thread,
        "Execution interrupted from Codex Remote.",
      );
      emit(hostId, threadId);
    },
    async sendPrompt(hostId: string, threadId: string, input: SendTurnInput) {
      const record = getRecord(hostId, threadId);
      state.prompts.push(input.text);
      record.runtime.phase = "running";
      record.runtime.statusCopy =
        input.mode === "steer"
          ? "Steering note delivered to the running turn."
          : "New turn started from the phone.";
      record.thread.status = { type: "active", activeFlags: [] };
      record.thread.updatedAt = epochSeconds();
      record.thread.turns.push({
        error: null,
        id: `turn-${record.thread.turns.length + 1}`,
        items: [
          {
            type: "userMessage",
            id: `user-${record.thread.turns.length + 1}`,
            content: [textInput(input.text)],
          },
          {
            type: "agentMessage",
            id: `agent-${record.thread.turns.length + 1}`,
            phase: null,
            text:
              input.mode === "steer"
                ? "Steering accepted. Folding the mobile direction into the active plan."
                : "New turn accepted. Preparing the next host action.",
          },
        ],
        status: "completed",
      });
      emit(hostId, threadId);
    },
    async revokeDevice(hostId: string, deviceId: string) {
      const group = state.deviceGroups.find(
        (entry) => entry.host.id === hostId,
      );
      const device = group?.devices.find((entry) => entry.id === deviceId);
      if (device) {
        device.trust = "revoked";
      }
    },
    subscribeToThread(hostId: string, threadId: string, listener) {
      const key = listenerKey(hostId, threadId);
      const bucket = listeners.get(key) ?? new Set();
      bucket.add(listener);
      listeners.set(key, bucket);
      listener(clone(getRecord(hostId, threadId)));
      return () => {
        const current = listeners.get(key);
        if (!current) {
          return;
        }
        current.delete(listener);
        if (current.size === 0) {
          listeners.delete(key);
        }
      };
    },
    inspect() {
      return {
        approvalsResolved: clone(state.approvalsResolved),
        interrupts: [...state.interrupts],
        prompts: [...state.prompts],
      };
    },
  };
}

function createState(options: MockGatewayOptions): MutableState {
  const now = epochSeconds();
  const nativeCapabilities =
    options.nativeCapabilities ??
    ({
      secureStore: true,
      qrScanner: true,
      fileImport: true,
      relaySockets: true,
    } satisfies NativeCapabilities);

  const studioHost: HostSummary = {
    detail: "Relay online through the home daemon",
    id: "host-studio",
    lastSeenAt: now - 45,
    name: "Studio MacBook",
    pairedAt: now - 7 * 24 * 60 * 60,
    platform: "macOS",
    relayStatus: "Relay protected",
    status: "online",
  };

  const linuxHost: HostSummary = {
    detail: "Secondary build host with no active approvals",
    id: "host-lab",
    lastSeenAt: now - 4 * 60,
    name: "Build Linux",
    pairedAt: now - 2 * 24 * 60 * 60,
    platform: "Linux",
    relayStatus: "Relay idle",
    status: "offline",
  };

  const iosThread = createIosThreadRecord(studioHost.id, now);
  const reviewThread = createReviewThreadRecord(studioHost.id, now);
  const linuxThread = createLinuxThreadRecord(linuxHost.id, now);

  return {
    approvalsResolved: [],
    deviceGroups: [
      {
        devices: [
          {
            id: "device-iphone-main",
            lastSeenAt: now - 30,
            name: "Val's iPhone",
            pairedAt: now - 7 * 24 * 60 * 60,
            transport: "Relay + E2EE",
            trust: "trusted",
          },
          {
            id: "device-ipad-review",
            lastSeenAt: now - 90 * 60,
            name: "Review iPad",
            pairedAt: now - 4 * 24 * 60 * 60,
            transport: "Relay standby",
            trust: "trusted",
          },
        ] satisfies PairedDevice[],
        host: studioHost,
      },
      {
        devices: [
          {
            id: "device-iphone-old",
            lastSeenAt: now - 10 * 24 * 60 * 60,
            name: "Old field phone",
            pairedAt: now - 20 * 24 * 60 * 60,
            transport: "Revoked",
            trust: "revoked",
          },
        ] satisfies PairedDevice[],
        host: linuxHost,
      },
    ],
    hosts: [studioHost, linuxHost],
    interrupts: [],
    prompts: [],
    session: {
      accountLabel: "val@openai.com",
      nativeCapabilities,
      pairingCode: "CDEX-6174",
      pairingUrl: "codex://remote/pair?code=CDEX-6174",
      signedIn: options.signedIn ?? true,
      workspaceLabel: "Codex Lab",
    },
    threadRecords: {
      [linuxHost.id]: {
        [linuxThread.thread.id]: linuxThread,
      },
      [studioHost.id]: {
        [iosThread.thread.id]: iosThread,
        [reviewThread.thread.id]: reviewThread,
      },
    },
  };
}

function createIosThreadRecord(
  hostId: string,
  now: number,
): RemoteThreadRecord {
  const commandApproval: CommandExecutionRequestApprovalParams = {
    additionalPermissions: permissionProfile({
      fileSystem: {
        read: ["/Users/val/.codex/worktrees/7759/codex/apps/mobile/src-tauri"],
        write: ["/Users/val/.codex/worktrees/7759/codex/apps/mobile/src-tauri"],
      },
      macos: null,
      network: null,
    }),
    approvalId: null,
    availableDecisions: ["accept", "acceptForSession", "decline", "cancel"],
    command: "pnpm tauri ios dev",
    commandActions: [],
    cwd: "/Users/val/.codex/worktrees/7759/codex/apps/mobile",
    itemId: "item-cmd-tauri-dev",
    proposedExecpolicyAmendment: null,
    proposedNetworkPolicyAmendments: null,
    reason: "Launch the native shell to validate the iOS pairing experience.",
    skillMetadata: null,
    threadId: "thread-ios",
    turnId: "turn-ios-2",
  };

  const approvals: RemoteApproval[] = [
    {
      decisions: ["accept", "acceptForSession", "decline", "cancel"],
      params: commandApproval,
      requestId: "approval-turn-shell",
      type: "command",
    },
  ];

  return {
    approvals,
    hostId,
    runtime: {
      composerMode: "steer",
      connection: "online",
      phase: "waitingOnApproval",
      statusCopy: "Waiting for a shell approval on the host.",
    },
    thread: {
      agentNickname: null,
      agentRole: null,
      cliVersion: "0.0.0-dev",
      createdAt: now - 3_600,
      cwd: "/Users/val/.codex/worktrees/7759/codex",
      ephemeral: false,
      gitInfo: null,
      id: "thread-ios",
      modelProvider: "openai",
      name: "Remote iOS daemon",
      path: null,
      preview: "Stage the Tauri iOS remote-control shell and host runtime.",
      source: "appServer",
      status: { type: "active", activeFlags: ["waitingOnApproval"] },
      turns: [
        {
          error: null,
          id: "turn-ios-1",
          items: [
            {
              content: [
                textInput("Create the mobile shell and keep the docs current."),
              ],
              id: "user-ios-1",
              type: "userMessage",
            },
            {
              id: "plan-ios-1",
              text: "1. Wire host runtime\n2. Build the Tauri shell\n3. Document the remote contract",
              type: "plan",
            },
            {
              id: "agent-ios-1",
              phase: null,
              text: "Host transport is in place. Moving into the mobile shell.",
              type: "agentMessage",
            },
          ],
          status: "completed",
        },
        {
          error: null,
          id: "turn-ios-2",
          items: [
            {
              command: "pnpm tauri ios dev",
              commandActions: [],
              cwd: "/Users/val/.codex/worktrees/7759/codex/apps/mobile",
              durationMs: null,
              exitCode: null,
              id: "cmd-ios-2",
              processId: "pty-982",
              status: "inProgress",
              aggregatedOutput:
                "Waiting on mobile host approval to launch the native shell.",
              type: "commandExecution",
            },
          ],
          status: "inProgress",
        },
      ],
      updatedAt: now - 90,
    },
  };
}

function createReviewThreadRecord(
  hostId: string,
  now: number,
): RemoteThreadRecord {
  return {
    approvals: [],
    hostId,
    runtime: {
      composerMode: "newTurn",
      connection: "online",
      phase: "completed",
      statusCopy: "Turn completed. Ready for a new request.",
    },
    thread: {
      agentNickname: null,
      agentRole: null,
      cliVersion: "0.0.0-dev",
      createdAt: now - 10_800,
      cwd: "/Users/val/.codex/worktrees/7759/codex",
      ephemeral: false,
      gitInfo: null,
      id: "thread-review",
      modelProvider: "openai",
      name: "Protocol polish",
      path: null,
      preview: "Stabilize approval decisions in app-server v2.",
      source: "exec",
      status: { type: "idle" },
      turns: [
        {
          error: null,
          id: "turn-review-1",
          items: [
            {
              content: [
                textInput(
                  "Promote availableDecisions to stable in app-server v2.",
                ),
              ],
              id: "user-review-1",
              type: "userMessage",
            },
            {
              id: "agent-review-1",
              phase: null,
              text: "Schema, docs, and focused protocol tests have been updated.",
              type: "agentMessage",
            },
          ],
          status: "completed",
        },
      ],
      updatedAt: now - 1_200,
    },
  };
}

function createLinuxThreadRecord(
  hostId: string,
  now: number,
): RemoteThreadRecord {
  return {
    approvals: [],
    hostId,
    runtime: {
      composerMode: "newTurn",
      connection: "offline",
      phase: "completed",
      statusCopy: "Host offline. Queueing is disabled until it reconnects.",
    },
    thread: {
      agentNickname: null,
      agentRole: null,
      cliVersion: "0.0.0-dev",
      createdAt: now - 86_400,
      cwd: "/srv/codex",
      ephemeral: false,
      gitInfo: null,
      id: "thread-linux",
      modelProvider: "openai",
      name: "Relay daemon diagnostics",
      path: null,
      preview: "Capture relay reconnect behavior on Linux.",
      source: "appServer",
      status: { type: "notLoaded" },
      turns: [
        {
          error: null,
          id: "turn-linux-1",
          items: [
            {
              content: [
                textInput(
                  "Summarize the last relay disconnects on the Linux host.",
                ),
              ],
              id: "user-linux-1",
              type: "userMessage",
            },
            {
              id: "agent-linux-1",
              phase: null,
              text: "Reconnect metrics were collected before the host went offline.",
              type: "agentMessage",
            },
          ],
          status: "completed",
        },
      ],
      updatedAt: now - 14_400,
    },
  };
}

function appendAgentMessage(thread: Thread, text: string) {
  const lastTurn = thread.turns.at(-1);
  if (!lastTurn) {
    return;
  }

  lastTurn.items.push({
    id: `agent-${lastTurn.items.length + 1}`,
    phase: null,
    text,
    type: "agentMessage",
  });
  thread.updatedAt = epochSeconds();
}

function listenerKey(hostId: string, threadId: string): string {
  return `${hostId}:${threadId}`;
}

function epochSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

function clone<T>(value: T): T {
  return structuredClone(value);
}

function textInput(text: string): UserInput {
  return {
    text,
    text_elements: [],
    type: "text",
  };
}

function permissionProfile(
  profile: AdditionalPermissionProfile,
): AdditionalPermissionProfile {
  return profile;
}
