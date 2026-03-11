import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "./App";
import { createMockGateway } from "../remote/mockGateway";

describe("Codex remote mobile shell", () => {
  it("shows the sign-in and pairing flow before a session exists", async () => {
    render(
      <App
        gateway={createMockGateway({
          signedIn: false,
        })}
        initialEntries={["/"]}
      />,
    );

    expect(
      await screen.findByRole("heading", { name: /sign in to codex remote/i }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: /continue with openai/i }),
    ).toBeVisible();
    expect(screen.getByText(/scan a host pairing code/i)).toBeVisible();
  });

  it("navigates from hosts to threads and shows the live approval sheet", async () => {
    const user = userEvent.setup();

    render(<App gateway={createMockGateway()} initialEntries={["/hosts"]} />);

    await user.click(
      await screen.findByRole("link", { name: /studio macbook/i }),
    );
    await user.click(
      await screen.findByRole("link", { name: /remote ios daemon/i }),
    );

    expect(
      await screen.findByRole("heading", { name: /remote ios daemon/i }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: /interrupt turn/i }),
    ).toBeVisible();
    expect(
      screen.getByRole("heading", { name: /approval required/i }),
    ).toBeVisible();
    expect(screen.getAllByText(/pnpm tauri ios dev/i)).toHaveLength(2);
  });

  it("routes approval and interrupt actions through the gateway", async () => {
    const user = userEvent.setup();
    const gateway = createMockGateway();

    render(
      <App
        gateway={gateway}
        initialEntries={["/hosts/host-studio/threads/thread-ios"]}
      />,
    );

    await waitFor(() =>
      expect(
        screen.getByRole("heading", { name: /remote ios daemon/i }),
      ).toBeVisible(),
    );

    await user.click(screen.getByRole("button", { name: /approve for turn/i }));
    await user.click(screen.getByRole("button", { name: /interrupt turn/i }));

    expect(gateway.inspect().approvalsResolved).toEqual([
      {
        decision: "accept",
        requestId: "approval-turn-shell",
      },
    ]);
    expect(gateway.inspect().interrupts).toEqual(["thread-ios"]);
  });
});
