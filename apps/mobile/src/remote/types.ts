import type {
  CommandExecutionApprovalDecision,
  CommandExecutionRequestApprovalParams,
  FileChangeApprovalDecision,
  FileChangeRequestApprovalParams,
  PermissionGrantScope,
  PermissionsRequestApprovalParams,
  Thread,
} from "./protocol";

export type NativeCapabilities = {
  secureStore: boolean;
  qrScanner: boolean;
  fileImport: boolean;
  relaySockets: boolean;
};

export type RemoteSession = {
  signedIn: boolean;
  accountLabel: string;
  workspaceLabel: string;
  pairingCode: string;
  pairingUrl: string;
  nativeCapabilities: NativeCapabilities;
};

export type HostSummary = {
  id: string;
  name: string;
  platform: string;
  status: "online" | "offline";
  relayStatus: string;
  detail: string;
  pairedAt: number;
  lastSeenAt: number;
};

export type PairedDevice = {
  id: string;
  name: string;
  trust: "trusted" | "revoked";
  pairedAt: number;
  lastSeenAt: number;
  transport: string;
};

export type DeviceGroup = {
  host: HostSummary;
  devices: PairedDevice[];
};

export type ComposerMode = "newTurn" | "steer";

export type ThreadRuntimeState = {
  connection: "online" | "offline";
  phase: "running" | "waitingOnApproval" | "completed";
  composerMode: ComposerMode;
  statusCopy: string;
};

export type CommandApproval = {
  type: "command";
  requestId: string;
  params: CommandExecutionRequestApprovalParams;
  decisions: CommandExecutionApprovalDecision[];
};

export type PermissionsApproval = {
  type: "permissions";
  requestId: string;
  params: PermissionsRequestApprovalParams;
  defaultScope: PermissionGrantScope;
};

export type FileChangeApproval = {
  type: "fileChange";
  requestId: string;
  params: FileChangeRequestApprovalParams;
  decisions: FileChangeApprovalDecision[];
};

export type RemoteApproval =
  | CommandApproval
  | FileChangeApproval
  | PermissionsApproval;

export type RemoteThreadRecord = {
  hostId: string;
  thread: Thread;
  approvals: RemoteApproval[];
  runtime: ThreadRuntimeState;
};

export type RemoteThreadRecordEvent = {
  hostId: string;
  threadId: string;
  record: RemoteThreadRecord;
};

export type ApprovalResolution = {
  requestId: string;
  decision: "accept" | "acceptForSession" | "decline" | "cancel";
  scope?: PermissionGrantScope;
};

export type SendTurnInput = {
  text: string;
  mode: ComposerMode;
};

export type GatewayInspection = {
  approvalsResolved: Array<{ requestId: string; decision: string }>;
  interrupts: string[];
  prompts: string[];
};

export type RelayConnectorSnapshot = {
  connectorMode: "localPreview";
  session: Omit<RemoteSession, "nativeCapabilities">;
  hosts: HostSummary[];
  deviceGroups: DeviceGroup[];
};

export type RemoteBootstrap = {
  session: RemoteSession;
  hosts: HostSummary[];
  deviceGroups: DeviceGroup[];
};

export interface RemoteGateway {
  getSession(): Promise<RemoteSession>;
  signIn(): Promise<void>;
  listHosts(): Promise<HostSummary[]>;
  listThreads(hostId: string): Promise<Thread[]>;
  getThread(hostId: string, threadId: string): Promise<RemoteThreadRecord>;
  listDeviceGroups(): Promise<DeviceGroup[]>;
  resolveApproval(
    hostId: string,
    threadId: string,
    resolution: ApprovalResolution,
  ): Promise<void>;
  interruptTurn(hostId: string, threadId: string): Promise<void>;
  sendPrompt(
    hostId: string,
    threadId: string,
    input: SendTurnInput,
  ): Promise<void>;
  revokeDevice(hostId: string, deviceId: string): Promise<void>;
  subscribeToThread(
    hostId: string,
    threadId: string,
    listener: (record: RemoteThreadRecord) => void,
  ): () => void;
  subscribeToBootstrap(listener: (bootstrap: RemoteBootstrap) => void): () => void;
  inspect(): GatewayInspection;
}
