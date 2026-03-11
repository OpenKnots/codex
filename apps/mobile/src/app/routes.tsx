import { useQueryClient } from "@tanstack/react-query";
import {
  startTransition,
  type FormEvent,
  useDeferredValue,
  useEffect,
  useEffectEvent,
  useState,
} from "react";
import {
  Link,
  NavLink,
  Navigate,
  Outlet,
  Route,
  Routes,
  useNavigate,
  useParams,
} from "react-router-dom";
import { selectAttachmentImport } from "../native/bridge";
import type {
  AdditionalPermissionProfile,
  Thread,
  ThreadItem,
  UserInput,
} from "../remote/protocol";
import {
  gatewayQueryKeys,
  useDeviceGroupsQuery,
  useGateway,
  useHostsQuery,
  useSessionQuery,
  useThreadQuery,
  useThreadsQuery,
} from "../remote/query";
import type {
  ComposerMode,
  HostSummary,
  NativeCapabilities,
  RemoteApproval,
  RemoteThreadRecord,
} from "../remote/types";
import {
  toThreadStoreKey,
  useLiveThreadStore,
} from "../thread/liveThreadStore";

export function AppRoutes() {
  return (
    <Routes>
      <Route path="/" element={<HomeRoute />} />
      <Route element={<RequireSignedIn />}>
        <Route element={<ShellLayout />}>
          <Route path="/hosts" element={<HostsRoute />} />
          <Route path="/hosts/:hostId" element={<ThreadsRoute />} />
          <Route
            path="/hosts/:hostId/threads/:threadId"
            element={<ThreadRoute />}
          />
          <Route path="/settings" element={<SettingsRoute />} />
        </Route>
      </Route>
      <Route path="*" element={<Navigate replace to="/" />} />
    </Routes>
  );
}

function HomeRoute() {
  const gateway = useGateway();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const sessionQuery = useSessionQuery();

  if (sessionQuery.isLoading || !sessionQuery.data) {
    return <LoadingScreen label="Checking account session" />;
  }

  if (sessionQuery.data.signedIn) {
    return <Navigate replace to="/hosts" />;
  }

  async function handleSignIn() {
    await gateway.signIn();
    await queryClient.invalidateQueries({ queryKey: gatewayQueryKeys.session });
    startTransition(() => {
      navigate("/hosts");
    });
  }

  return (
    <div className="screen auth-screen">
      <div className="auth-hero">
        <p className="eyebrow">Foreground remote control</p>
        <h1>Sign in to Codex Remote</h1>
        <p className="lede">
          Pair one iPhone to your Codex host, approve commands in-flight, and
          steer active turns without exposing a public port.
        </p>
      </div>

      <section className="card stack gap-lg">
        <div className="stack gap-sm">
          <span className="section-label">Account</span>
          <button
            className="primary-button"
            onClick={handleSignIn}
            type="button"
          >
            Continue with OpenAI
          </button>
        </div>

        <div className="stack gap-sm">
          <span className="section-label">Pairing</span>
          <h2>Scan a host pairing code</h2>
          <p className="muted">
            Use <code>codex remote pair</code> on the host, then scan the
            short-lived QR or open the deep link below.
          </p>
          <div className="pairing-code-row">
            <div>
              <span className="micro-label">Pairing code</span>
              <div className="pairing-code">
                {sessionQuery.data.pairingCode}
              </div>
            </div>
            <div>
              <span className="micro-label">Deep link</span>
              <div className="pairing-link">{sessionQuery.data.pairingUrl}</div>
            </div>
          </div>
        </div>

        <div className="stack gap-sm">
          <span className="section-label">Native readiness</span>
          <CapabilityGrid capabilities={sessionQuery.data.nativeCapabilities} />
        </div>
      </section>
    </div>
  );
}

function RequireSignedIn() {
  const sessionQuery = useSessionQuery();
  if (sessionQuery.isLoading || !sessionQuery.data) {
    return <LoadingScreen label="Restoring remote session" />;
  }

  if (!sessionQuery.data.signedIn) {
    return <Navigate replace to="/" />;
  }

  return <Outlet />;
}

function ShellLayout() {
  const sessionQuery = useSessionQuery();
  if (!sessionQuery.data) {
    return null;
  }

  return (
    <div className="app-shell">
      <header className="shell-header">
        <div>
          <p className="eyebrow">Codex Remote</p>
          <h1>{sessionQuery.data.workspaceLabel}</h1>
        </div>
        <div className="account-chip">{sessionQuery.data.accountLabel}</div>
      </header>
      <main className="shell-main">
        <Outlet />
      </main>
      <nav className="bottom-nav" aria-label="Primary">
        <NavLink className={navClassName} to="/hosts">
          Hosts
        </NavLink>
        <NavLink className={navClassName} to="/settings">
          Settings
        </NavLink>
      </nav>
    </div>
  );
}

function HostsRoute() {
  const hostsQuery = useHostsQuery();
  if (hostsQuery.isLoading || !hostsQuery.data) {
    return <LoadingScreen label="Loading paired hosts" />;
  }

  const onlineCount = hostsQuery.data.filter(
    (host) => host.status === "online",
  ).length;

  return (
    <div className="screen stack gap-lg">
      <ScreenIntro
        eyebrow="Hosts"
        title="Paired machines"
        detail={`${onlineCount} online host${onlineCount === 1 ? "" : "s"} ready for remote control.`}
      />

      <div className="stack gap-md">
        {hostsQuery.data.map((host) => (
          <Link
            className="card host-card"
            key={host.id}
            to={`/hosts/${host.id}`}
          >
            <div className="host-card-top">
              <div>
                <h2>{host.name}</h2>
                <p className="muted">{host.detail}</p>
              </div>
              <StatusPill tone={host.status === "online" ? "success" : "muted"}>
                {host.status}
              </StatusPill>
            </div>
            <div className="host-card-meta">
              <span>{host.platform}</span>
              <span>{host.relayStatus}</span>
              <span>Seen {formatRelativeTime(host.lastSeenAt)}</span>
            </div>
          </Link>
        ))}
      </div>
    </div>
  );
}

function ThreadsRoute() {
  const { hostId } = useParams();
  if (!hostId) {
    return <Navigate replace to="/hosts" />;
  }

  const [search, setSearch] = useState("");
  const deferredSearch = useDeferredValue(search);
  const hostsQuery = useHostsQuery();
  const threadsQuery = useThreadsQuery(hostId);
  const host = hostsQuery.data?.find((entry) => entry.id === hostId);

  if (threadsQuery.isLoading || !threadsQuery.data) {
    return <LoadingScreen label="Loading host threads" />;
  }

  const normalizedSearch = deferredSearch.trim().toLowerCase();
  const filteredThreads = threadsQuery.data.filter((thread) => {
    const searchable = `${thread.name ?? ""} ${thread.preview}`.toLowerCase();
    return (
      normalizedSearch.length === 0 || searchable.includes(normalizedSearch)
    );
  });

  return (
    <div className="screen stack gap-lg">
      <Link className="back-link" to="/hosts">
        All hosts
      </Link>
      <ScreenIntro
        eyebrow="Threads"
        title={host?.name ?? "Host"}
        detail="Thread titles come from `Thread.name`; preview lines stay compact for mobile scanning."
      />

      <label className="search-field">
        <span className="micro-label">Search</span>
        <input
          aria-label="Search threads"
          onChange={(event) => setSearch(event.target.value)}
          placeholder="Find a live thread"
          type="search"
          value={search}
        />
      </label>

      <div className="stack gap-md">
        {filteredThreads.map((thread) => (
          <Link
            className="card thread-list-card"
            key={thread.id}
            to={`/hosts/${hostId}/threads/${thread.id}`}
          >
            <div className="thread-list-head">
              <h2>{thread.name ?? "Untitled thread"}</h2>
              <StatusPill tone={statusTone(thread)}>
                {threadStatusLabel(thread)}
              </StatusPill>
            </div>
            <p className="muted">{thread.preview}</p>
            <div className="thread-list-meta">
              <span>{thread.modelProvider}</span>
              <span>{formatRelativeTime(thread.updatedAt)}</span>
            </div>
          </Link>
        ))}
      </div>
    </div>
  );
}

function ThreadRoute() {
  const { hostId, threadId } = useParams();
  const gateway = useGateway();
  const queryClient = useQueryClient();
  const [composerMode, setComposerMode] = useState<ComposerMode>("steer");
  const setActiveThread = useLiveThreadStore((state) => state.setActiveThread);
  const setComposerDraft = useLiveThreadStore(
    (state) => state.setComposerDraft,
  );
  const upsertSnapshot = useLiveThreadStore((state) => state.upsertSnapshot);

  if (!hostId || !threadId) {
    return <Navigate replace to="/hosts" />;
  }

  const resolvedHostId = hostId;
  const resolvedThreadId = threadId;
  const key = toThreadStoreKey(resolvedHostId, resolvedThreadId);
  const threadQuery = useThreadQuery(resolvedHostId, resolvedThreadId);
  const hostsQuery = useHostsQuery();
  const snapshot = useLiveThreadStore((state) => state.snapshots[key]);
  const draft = useLiveThreadStore((state) => state.drafts[key] ?? "");
  const host = hostsQuery.data?.find((entry) => entry.id === resolvedHostId);

  useEffect(() => {
    if (!threadQuery.data) {
      return;
    }

    upsertSnapshot(key, threadQuery.data);
    setActiveThread(key);
    return () => {
      setActiveThread(null);
    };
  }, [key, setActiveThread, threadQuery.data, upsertSnapshot]);

  const applyIncomingRecord = useEffectEvent((record: RemoteThreadRecord) => {
    upsertSnapshot(key, record);
  });

  useEffect(() => {
    return gateway.subscribeToThread(
      resolvedHostId,
      resolvedThreadId,
      applyIncomingRecord,
    );
  }, [gateway, resolvedHostId, resolvedThreadId]);

  const record = snapshot ?? threadQuery.data;
  useEffect(() => {
    if (!record) {
      return;
    }
    setComposerMode(record.runtime.composerMode);
  }, [record]);

  if (threadQuery.isLoading || !record) {
    return <LoadingScreen label="Loading live thread" />;
  }

  const items = flattenThreadItems(record.thread);
  const hasActiveTurn =
    record.runtime.phase === "running" ||
    record.runtime.phase === "waitingOnApproval";
  const isHostOnline = record.runtime.connection === "online";
  const canInterrupt = isHostOnline && hasActiveTurn;
  const canQueuePrompt = isHostOnline;

  async function handleResolveApproval(
    approval: RemoteApproval,
    decision: "accept" | "acceptForSession" | "decline" | "cancel",
  ) {
    await gateway.resolveApproval(resolvedHostId, resolvedThreadId, {
      decision,
      requestId: approval.requestId,
      scope:
        approval.type === "permissions"
          ? decision === "acceptForSession"
            ? "session"
            : approval.defaultScope
          : undefined,
    });
    await queryClient.invalidateQueries({
      queryKey: gatewayQueryKeys.thread(resolvedHostId, resolvedThreadId),
    });
    await queryClient.invalidateQueries({
      queryKey: gatewayQueryKeys.threads(resolvedHostId),
    });
  }

  async function handleInterrupt() {
    if (!canInterrupt) {
      return;
    }
    await gateway.interruptTurn(resolvedHostId, resolvedThreadId);
    await queryClient.invalidateQueries({
      queryKey: gatewayQueryKeys.thread(resolvedHostId, resolvedThreadId),
    });
    await queryClient.invalidateQueries({
      queryKey: gatewayQueryKeys.threads(resolvedHostId),
    });
  }

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!canQueuePrompt) {
      return;
    }
    const prompt = draft.trim();
    if (prompt.length === 0) {
      return;
    }

    await gateway.sendPrompt(resolvedHostId, resolvedThreadId, {
      mode: composerMode,
      text: prompt,
    });
    await queryClient.invalidateQueries({
      queryKey: gatewayQueryKeys.thread(resolvedHostId, resolvedThreadId),
    });
    await queryClient.invalidateQueries({
      queryKey: gatewayQueryKeys.threads(resolvedHostId),
    });
    startTransition(() => {
      setComposerDraft(key, "");
    });
  }

  async function handleImport() {
    if (!canQueuePrompt) {
      return;
    }
    const filePath = await selectAttachmentImport();
    if (!filePath) {
      return;
    }

    setComposerDraft(
      key,
      draft.length === 0
        ? `[import] ${filePath}`
        : `${draft}\n[import] ${filePath}`,
    );
  }

  return (
    <div className="screen stack gap-lg">
      <div className="thread-hero">
        <div>
          <Link className="back-link" to={`/hosts/${resolvedHostId}`}>
            {host?.name ?? "Host"} threads
          </Link>
          <h1>{record.thread.name ?? "Untitled thread"}</h1>
          <p className="muted">{record.thread.preview}</p>
        </div>
        <button
          className="danger-button"
          disabled={!canInterrupt}
          onClick={handleInterrupt}
          type="button"
        >
          Interrupt turn
        </button>
      </div>

      <div className={isHostOnline ? "status-banner live" : "status-banner"}>
        <div className="status-banner-head">
          <div className="status-banner-copy">
            <p className="eyebrow">Live runtime</p>
            <h2>{threadRuntimeLabel(record.runtime.phase)}</h2>
          </div>
          <div className="status-banner-chips">
            <StatusPill tone={isHostOnline ? "success" : "warning"}>
              {isHostOnline ? "Host online" : "Host offline"}
            </StatusPill>
            {record.approvals.length > 0 ? (
              <StatusPill tone="warning">
                {`${record.approvals.length} approval${record.approvals.length === 1 ? "" : "s"}`}
              </StatusPill>
            ) : null}
          </div>
        </div>
        <p>{record.runtime.statusCopy}</p>
        <div className="status-banner-meta">
          <span>{record.thread.modelProvider}</span>
          <span>
            {hasActiveTurn ? "Steering enabled" : "Ready for the next turn"}
          </span>
        </div>
      </div>

      {record.approvals.length > 0 ? (
        <section className="approval-sheet stack gap-md">
          <div className="approval-head">
            <div>
              <p className="eyebrow">Action required</p>
              <h2>Approval required</h2>
            </div>
            <StatusPill tone="warning">{`${record.approvals.length} pending`}</StatusPill>
          </div>

          {record.approvals.map((approval) => (
            <div className="approval-card" key={approval.requestId}>
              <div className="approval-card-head">
                <div>
                  <p className="micro-label">{approvalKindLabel(approval)}</p>
                  <h3>{approvalTitle(approval)}</h3>
                </div>
                <StatusPill tone="warning">Pending</StatusPill>
              </div>

              <code>{approvalCode(approval)}</code>
              <p className="muted">{approvalReason(approval)}</p>

              <div className="approval-facts">
                {approvalFacts(approval).map((fact) => (
                  <span className="approval-fact" key={fact}>
                    {fact}
                  </span>
                ))}
              </div>

              <div className="button-row">
                {supportsSessionApproval(approval) ? (
                  <button
                    className="secondary-button"
                    onClick={() =>
                      handleResolveApproval(approval, "acceptForSession")
                    }
                    type="button"
                  >
                    Approve for session
                  </button>
                ) : null}
                <button
                  className="primary-button"
                  onClick={() => handleResolveApproval(approval, "accept")}
                  type="button"
                >
                  Approve for turn
                </button>
                <button
                  className="ghost-button"
                  onClick={() => handleResolveApproval(approval, "decline")}
                  type="button"
                >
                  Decline
                </button>
              </div>
            </div>
          ))}
        </section>
      ) : null}

      <section className="stack gap-md">
        <div className="timeline-header">
          <p className="eyebrow">Timeline</p>
          <span className="muted">
            {record.runtime.phase === "completed"
              ? "Turn completed"
              : record.runtime.phase === "waitingOnApproval"
                ? "Waiting for approval"
                : "Live stream active"}
          </span>
        </div>
        <div className="timeline">
          {items.map((item) => (
            <TimelineCard item={item} key={item.id} />
          ))}
        </div>
      </section>

      <form className="composer" onSubmit={handleSubmit}>
        <div
          className="composer-mode-row"
          role="tablist"
          aria-label="Composer mode"
        >
          <button
            className={
              !canQueuePrompt
                ? "mode-chip disabled"
                : composerMode === "newTurn"
                  ? "mode-chip active"
                  : "mode-chip"
            }
            disabled={!canQueuePrompt}
            onClick={() => setComposerMode("newTurn")}
            type="button"
          >
            New turn
          </button>
          <button
            className={
              composerMode === "steer"
                ? "mode-chip active"
                : canQueuePrompt && hasActiveTurn
                  ? "mode-chip"
                  : "mode-chip disabled"
            }
            disabled={!canQueuePrompt || !hasActiveTurn}
            onClick={() => setComposerMode("steer")}
            type="button"
          >
            Steer in-flight
          </button>
        </div>

        <label className="composer-field">
          <span className="micro-label">Composer</span>
          <textarea
            disabled={!canQueuePrompt}
            onChange={(event) => setComposerDraft(key, event.target.value)}
            placeholder={
              !canQueuePrompt
                ? "Wait for the host to reconnect before sending the next turn."
                : hasActiveTurn
                ? "Refine the running turn or start the next one."
                : "Queue up the next turn for this thread."
            }
            rows={4}
            value={draft}
          />
        </label>

        {!canQueuePrompt ? (
          <p className="muted composer-hint">
            Queueing is unavailable while the host is offline.
          </p>
        ) : null}

        <div className="button-row">
          <button
            className="ghost-button"
            disabled={!canQueuePrompt}
            onClick={handleImport}
            type="button"
          >
            Import attachment
          </button>
          <button className="primary-button" disabled={!canQueuePrompt} type="submit">
            Send
          </button>
        </div>
      </form>
    </div>
  );
}

function SettingsRoute() {
  const gateway = useGateway();
  const queryClient = useQueryClient();
  const groupsQuery = useDeviceGroupsQuery();

  if (groupsQuery.isLoading || !groupsQuery.data) {
    return <LoadingScreen label="Loading device registry" />;
  }

  async function handleRevoke(hostId: string, deviceId: string) {
    await gateway.revokeDevice(hostId, deviceId);
    startTransition(() => {
      void queryClient.invalidateQueries({
        queryKey: gatewayQueryKeys.devices,
      });
    });
  }

  return (
    <div className="screen stack gap-lg">
      <ScreenIntro
        eyebrow="Settings"
        title="Trusted devices"
        detail="Per-device revocation stays local to the paired host and keeps the relay stateless."
      />

      <div className="stack gap-md">
        {groupsQuery.data.map((group) => (
          <section className="card stack gap-md" key={group.host.id}>
            <div className="host-card-top">
              <div>
                <h2>{group.host.name}</h2>
                <p className="muted">{group.host.relayStatus}</p>
              </div>
              <StatusPill
                tone={group.host.status === "online" ? "success" : "muted"}
              >
                {group.host.status}
              </StatusPill>
            </div>
            {group.devices.map((device) => (
              <div className="device-row" key={device.id}>
                <div>
                  <h3>{device.name}</h3>
                  <p className="muted">
                    {device.transport} • Last seen{" "}
                    {formatRelativeTime(device.lastSeenAt)}
                  </p>
                </div>
                {device.trust === "trusted" ? (
                  <button
                    className="ghost-button"
                    onClick={() => handleRevoke(group.host.id, device.id)}
                    type="button"
                  >
                    Revoke
                  </button>
                ) : (
                  <StatusPill tone="warning">Revoked</StatusPill>
                )}
              </div>
            ))}
          </section>
        ))}
      </div>
    </div>
  );
}

function ScreenIntro({
  detail,
  eyebrow,
  title,
}: {
  detail: string;
  eyebrow: string;
  title: string;
}) {
  return (
    <div className="stack gap-sm">
      <p className="eyebrow">{eyebrow}</p>
      <h1>{title}</h1>
      <p className="lede">{detail}</p>
    </div>
  );
}

function LoadingScreen({ label }: { label: string }) {
  return (
    <div className="screen loading-screen">
      <div className="card loading-card">
        <p className="eyebrow">Codex Remote</p>
        <h1>{label}</h1>
      </div>
    </div>
  );
}

function CapabilityGrid({
  capabilities,
}: {
  capabilities: NativeCapabilities;
}) {
  const entries: Array<[string, boolean]> = [
    ["Keychain storage", capabilities.secureStore],
    ["QR pairing", capabilities.qrScanner],
    ["File import", capabilities.fileImport],
    ["Relay sockets", capabilities.relaySockets],
  ];

  return (
    <div className="capability-grid">
      {entries.map(([label, enabled]) => (
        <div
          className={enabled ? "capability-chip enabled" : "capability-chip"}
          key={label}
        >
          <span>{label}</span>
          <strong>{enabled ? "ready" : "web preview"}</strong>
        </div>
      ))}
    </div>
  );
}

function TimelineCard({ item }: { item: ThreadItem }) {
  switch (item.type) {
    case "userMessage":
      return (
        <article className="timeline-card user">
          <p className="micro-label">User</p>
          <p>{item.content.map(contentToText).join(" ")}</p>
        </article>
      );
    case "agentMessage":
      return (
        <article className="timeline-card agent">
          <p className="micro-label">Agent</p>
          <p>{item.text}</p>
        </article>
      );
    case "plan":
      return (
        <article className="timeline-card plan">
          <p className="micro-label">Plan</p>
          <p>{item.text}</p>
        </article>
      );
    case "reasoning":
      return (
        <article className="timeline-card reasoning">
          <p className="micro-label">Reasoning</p>
          <p>{item.summary.join(" ")}</p>
        </article>
      );
    case "commandExecution":
      return (
        <article className="timeline-card command">
          <p className="micro-label">Command execution</p>
          <code>{item.command}</code>
          <p className="muted">{item.aggregatedOutput ?? "No output yet."}</p>
        </article>
      );
    case "fileChange":
      return (
        <article className="timeline-card file">
          <p className="micro-label">File changes</p>
          <p>{item.changes.length} staged edit(s) ready to apply.</p>
        </article>
      );
    case "webSearch":
      return (
        <article className="timeline-card neutral">
          <p className="micro-label">Web search</p>
          <p>{item.query}</p>
        </article>
      );
    default:
      return (
        <article className="timeline-card neutral">
          <p className="micro-label">{item.type}</p>
          <p>Live thread item captured from the host runtime.</p>
        </article>
      );
  }
}

function threadRuntimeLabel(phase: RemoteThreadRecord["runtime"]["phase"]) {
  switch (phase) {
    case "running":
      return "Turn running";
    case "waitingOnApproval":
      return "Waiting on approval";
    case "completed":
      return "Ready for input";
  }
}

function approvalKindLabel(approval: RemoteApproval): string {
  switch (approval.type) {
    case "command":
      return "Command approval";
    case "fileChange":
      return "File change approval";
    case "permissions":
      return "Permission approval";
  }
}

function approvalTitle(approval: RemoteApproval): string {
  switch (approval.type) {
    case "command":
      return approval.params.command ?? "Shell command pending approval";
    case "fileChange":
      return approval.params.grantRoot
        ? "Allow file edits under a session root"
        : "Allow file edits on the host";
    case "permissions":
      return "Grant additional host permissions";
  }
}

function approvalCode(approval: RemoteApproval): string {
  switch (approval.type) {
    case "command":
      return approval.params.command ?? "Unknown command";
    case "fileChange":
      return approval.params.grantRoot ?? "File edits requested by the host.";
    case "permissions":
      return permissionSummary(approval.params.permissions);
  }
}

function approvalReason(approval: RemoteApproval): string {
  switch (approval.type) {
    case "command":
      return approval.params.reason ?? "Shell approval requested.";
    case "fileChange":
      return approval.params.reason ?? "File changes are waiting for approval.";
    case "permissions":
      return approval.params.reason ?? "Additional host permissions requested.";
  }
}

function approvalFacts(approval: RemoteApproval): string[] {
  switch (approval.type) {
    case "command": {
      const facts = [];
      if (approval.params.cwd) {
        facts.push(`cwd ${approval.params.cwd}`);
      }
      const extraPermissions = permissionSummary(
        approval.params.additionalPermissions,
      );
      if (extraPermissions !== "No additional permissions.") {
        facts.push(extraPermissions);
      }
      return facts;
    }
    case "fileChange":
      return approval.params.grantRoot
        ? [`root ${approval.params.grantRoot}`]
        : ["Host file edits"];
    case "permissions":
      return [permissionSummary(approval.params.permissions)];
  }
}

function supportsSessionApproval(approval: RemoteApproval): boolean {
  switch (approval.type) {
    case "command":
    case "fileChange":
      return approval.decisions.includes("acceptForSession");
    case "permissions":
      return true;
  }
}

function permissionSummary(
  permissions: AdditionalPermissionProfile | null | undefined,
): string {
  if (!permissions) {
    return "No additional permissions.";
  }

  const facts = [];
  const reads = permissions.fileSystem?.read?.length ?? 0;
  const writes = permissions.fileSystem?.write?.length ?? 0;
  if (reads > 0) {
    facts.push(`${reads} read root${reads === 1 ? "" : "s"}`);
  }
  if (writes > 0) {
    facts.push(`${writes} write root${writes === 1 ? "" : "s"}`);
  }
  if (permissions.network) {
    facts.push("network access");
  }
  if (permissions.macos) {
    if (permissions.macos.preferences !== "none") {
      facts.push("macOS preferences");
    }
    if (permissions.macos.automations !== "none") {
      facts.push("macOS automations");
    }
    if (permissions.macos.accessibility) {
      facts.push("Accessibility");
    }
    if (permissions.macos.calendar) {
      facts.push("Calendar");
    }
  }

  return facts.length > 0
    ? facts.join(" • ")
    : "Additional host permissions requested.";
}

function StatusPill({
  children,
  tone,
}: {
  children: string;
  tone: "muted" | "success" | "warning";
}) {
  const className =
    tone === "success"
      ? "status-pill success"
      : tone === "warning"
        ? "status-pill warning"
        : "status-pill muted";
  return <span className={className}>{children}</span>;
}

function contentToText(input: UserInput): string {
  if ("text" in input && typeof input.text === "string") {
    return input.text;
  }
  if ("path" in input && typeof input.path === "string") {
    return input.path;
  }
  if ("url" in input && typeof input.url === "string") {
    return input.url;
  }
  if ("name" in input && typeof input.name === "string") {
    return input.name;
  }
  return "";
}

function flattenThreadItems(thread: Thread): ThreadItem[] {
  const items: ThreadItem[] = [];
  for (const turn of thread.turns) {
    items.push(...turn.items);
  }
  return items;
}

function navClassName({ isActive }: { isActive: boolean }) {
  return isActive ? "bottom-nav-link active" : "bottom-nav-link";
}

function threadStatusLabel(thread: Thread): string {
  switch (thread.status.type) {
    case "active":
      return thread.status.activeFlags.includes("waitingOnApproval")
        ? "Approval"
        : "Running";
    case "idle":
      return "Ready";
    case "notLoaded":
      return "Offline";
    case "systemError":
      return "Error";
  }
}

function statusTone(thread: Thread): "muted" | "success" | "warning" {
  switch (thread.status.type) {
    case "active":
      return thread.status.activeFlags.includes("waitingOnApproval")
        ? "warning"
        : "success";
    case "idle":
      return "success";
    case "notLoaded":
    case "systemError":
      return "muted";
  }
}

function formatRelativeTime(epoch: number): string {
  const delta = epoch - Math.floor(Date.now() / 1000);
  const formatter = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

  if (Math.abs(delta) < 60) {
    return formatter.format(delta, "second");
  }
  if (Math.abs(delta) < 3_600) {
    return formatter.format(Math.round(delta / 60), "minute");
  }
  if (Math.abs(delta) < 86_400) {
    return formatter.format(Math.round(delta / 3_600), "hour");
  }
  return formatter.format(Math.round(delta / 86_400), "day");
}
