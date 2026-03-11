import type {
  ApprovalResolution,
  NativeCapabilities,
  RelayConnectorSnapshot,
  RemoteThreadRecordEvent,
  RemoteThreadRecord,
  SendTurnInput,
} from "../remote/types";
import type { Thread } from "../remote/protocol";

const fallbackCapabilities: NativeCapabilities = {
  secureStore: false,
  qrScanner: false,
  fileImport: false,
  relaySockets: false,
};

export async function readNativeCapabilities(): Promise<NativeCapabilities> {
  try {
    return await invokeNative<NativeCapabilities>("read_native_capabilities");
  } catch {
    return fallbackCapabilities;
  }
}

export async function selectAttachmentImport(): Promise<string | null> {
  try {
    return await invokeNative<string | null>("pick_attachment_import");
  } catch {
    return null;
  }
}

export async function readRemoteConnectorSnapshot(): Promise<RelayConnectorSnapshot | null> {
  try {
    return await invokeNative<RelayConnectorSnapshot | null>(
      "read_remote_connector_snapshot",
    );
  } catch {
    return null;
  }
}

export async function startRemoteConnectorStream(): Promise<void> {
  await invokeNative("start_remote_connector_stream");
}

export async function stopRemoteConnectorStream(): Promise<void> {
  await invokeNative("stop_remote_connector_stream");
}

export async function listenRemoteConnectorSnapshots(
  listener: (payload: RelayConnectorSnapshot) => void,
): Promise<() => void> {
  try {
    return await listenNativeEvent<RelayConnectorSnapshot>(
      "remote-connector-snapshot",
      listener,
    );
  } catch {
    return () => {};
  }
}

export async function listRemoteThreads(hostId: string): Promise<Thread[] | null> {
  try {
    return await invokeNative<Thread[] | null>("list_remote_threads", { hostId });
  } catch {
    return null;
  }
}

export async function readRemoteThreadRecord(
  hostId: string,
  threadId: string,
): Promise<RemoteThreadRecord | null> {
  try {
    return await invokeNative<RemoteThreadRecord | null>(
      "read_remote_thread_record",
      { hostId, threadId },
    );
  } catch {
    return null;
  }
}

export async function sendRemotePrompt(
  hostId: string,
  threadId: string,
  input: SendTurnInput,
): Promise<void> {
  await invokeNative("send_remote_prompt", { hostId, input, threadId });
}

export async function interruptRemoteTurn(
  hostId: string,
  threadId: string,
): Promise<void> {
  await invokeNative("interrupt_remote_turn", { hostId, threadId });
}

export async function startRemoteThreadStream(
  hostId: string,
  threadId: string,
): Promise<void> {
  await invokeNative("start_remote_thread_stream", { hostId, threadId });
}

export async function stopRemoteThreadStream(
  hostId: string,
  threadId: string,
): Promise<void> {
  await invokeNative("stop_remote_thread_stream", { hostId, threadId });
}

export async function resolveRemoteApproval(
  hostId: string,
  threadId: string,
  resolution: ApprovalResolution,
): Promise<void> {
  await invokeNative("resolve_remote_approval", {
    hostId,
    resolution,
    threadId,
  });
}

export async function listenRemoteThreadRecords(
  listener: (payload: RemoteThreadRecordEvent) => void,
): Promise<() => void> {
  try {
    return await listenNativeEvent<RemoteThreadRecordEvent>(
      "remote-thread-record",
      listener,
    );
  } catch {
    return () => {};
  }
}

function invokeNative<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  const invoke = (
    window as Window & {
      __TAURI_INTERNALS__?: {
        invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<T>;
      };
    }
  ).__TAURI_INTERNALS__?.invoke;

  if (!invoke) {
    return Promise.reject(new Error("Tauri runtime unavailable."));
  }

  return invoke(command, args);
}

async function listenNativeEvent<T>(
  event: string,
  listener: (payload: T) => void,
): Promise<() => void> {
  const internals = window as Window & {
    __TAURI_EVENT_PLUGIN_INTERNALS__?: {
      unregisterListener?: (eventName: string, eventId: number) => void;
    };
    __TAURI_INTERNALS__?: {
      invoke?: <U>(cmd: string, args?: Record<string, unknown>) => Promise<U>;
      transformCallback?: (
        callback?: (payload: {
          event: string;
          id: number;
          payload: T;
        }) => void,
      ) => number;
    };
  };
  const invoke = internals.__TAURI_INTERNALS__?.invoke;
  const transformCallback = internals.__TAURI_INTERNALS__?.transformCallback;

  if (!invoke || !transformCallback) {
    return () => {};
  }

  const eventId = await invoke<number>("plugin:event|listen", {
    event,
    handler: transformCallback((payload) => {
      listener(payload.payload);
    }),
    target: {
      kind: "Any",
    },
  });

  return () => {
    internals.__TAURI_EVENT_PLUGIN_INTERNALS__?.unregisterListener?.(
      event,
      eventId,
    );
    void invoke("plugin:event|unlisten", {
      event,
      eventId,
    });
  };
}
