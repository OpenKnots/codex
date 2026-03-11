import { create } from "zustand";
import type { RemoteThreadRecord } from "../remote/types";

type LiveThreadState = {
  activeThreadKey: string | null;
  drafts: Record<string, string>;
  snapshots: Record<string, RemoteThreadRecord>;
  setActiveThread(threadKey: string | null): void;
  setComposerDraft(threadKey: string, draft: string): void;
  upsertSnapshot(threadKey: string, snapshot: RemoteThreadRecord): void;
};

export function toThreadStoreKey(hostId: string, threadId: string): string {
  return `${hostId}:${threadId}`;
}

export const useLiveThreadStore = create<LiveThreadState>((set) => ({
  activeThreadKey: null,
  drafts: {},
  snapshots: {},
  setActiveThread(activeThreadKey) {
    set({ activeThreadKey });
  },
  setComposerDraft(threadKey, draft) {
    set((state) => ({
      drafts: {
        ...state.drafts,
        [threadKey]: draft,
      },
    }));
  },
  upsertSnapshot(threadKey, snapshot) {
    set((state) => ({
      snapshots: {
        ...state.snapshots,
        [threadKey]: snapshot,
      },
    }));
  },
}));
