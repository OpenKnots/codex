import type { Thread } from "./protocol";
import type {
  ApprovalResolution,
  RemoteBootstrap,
  RemoteThreadRecord,
  SendTurnInput,
} from "./types";

export const relayMethods = {
  approvalResolve: "approval/resolve",
  bootstrapGet: "bootstrap/get",
  deviceRevoke: "device/revoke",
  sessionSignIn: "session/signIn",
  threadList: "thread/list",
  threadRead: "thread/read",
  threadSubscribe: "thread/subscribe",
  threadUnsubscribe: "thread/unsubscribe",
  turnInterrupt: "turn/interrupt",
  turnPrompt: "turn/prompt",
} as const;

export type RelayMethod = (typeof relayMethods)[keyof typeof relayMethods];

export type RelayMethodParams = {
  [relayMethods.approvalResolve]: {
    hostId: string;
    threadId: string;
    resolution: ApprovalResolution;
  };
  [relayMethods.bootstrapGet]: undefined;
  [relayMethods.deviceRevoke]: {
    hostId: string;
    deviceId: string;
  };
  [relayMethods.sessionSignIn]: undefined;
  [relayMethods.threadList]: {
    hostId: string;
  };
  [relayMethods.threadRead]: {
    hostId: string;
    threadId: string;
  };
  [relayMethods.threadSubscribe]: {
    hostId: string;
    threadId: string;
  };
  [relayMethods.threadUnsubscribe]: {
    hostId: string;
    threadId: string;
  };
  [relayMethods.turnInterrupt]: {
    hostId: string;
    threadId: string;
  };
  [relayMethods.turnPrompt]: {
    hostId: string;
    threadId: string;
    input: SendTurnInput;
  };
};

export type RelayMethodResult = {
  [relayMethods.approvalResolve]: void;
  [relayMethods.bootstrapGet]: RemoteBootstrap;
  [relayMethods.deviceRevoke]: RemoteBootstrap;
  [relayMethods.sessionSignIn]: RemoteBootstrap;
  [relayMethods.threadList]: Thread[];
  [relayMethods.threadRead]: RemoteThreadRecord;
  [relayMethods.threadSubscribe]: void;
  [relayMethods.threadUnsubscribe]: void;
  [relayMethods.turnInterrupt]: void;
  [relayMethods.turnPrompt]: void;
};

export type RelayServerNotification =
  | {
      type: "bootstrap/update";
      bootstrap: RemoteBootstrap;
    }
  | {
      type: "thread/update";
      hostId: string;
      threadId: string;
      record: RemoteThreadRecord;
    };

export type RelayRequestEnvelope<M extends RelayMethod = RelayMethod> = {
  type: "request";
  requestId: string;
  method: M;
} & (RelayMethodParams[M] extends undefined
  ? { params?: undefined }
  : { params: RelayMethodParams[M] });

export type RelayResponseEnvelope<M extends RelayMethod = RelayMethod> =
  | {
      type: "response";
      requestId: string;
      ok: true;
      result: RelayMethodResult[M];
    }
  | {
      type: "response";
      requestId: string;
      ok: false;
      error: string;
    };

export type RelayWireEnvelope =
  | RelayRequestEnvelope
  | RelayResponseEnvelope
  | RelayServerNotification;
