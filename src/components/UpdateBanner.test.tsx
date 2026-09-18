import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import UpdateBanner from "./UpdateBanner";

const { check, downloadAndInstall, relaunch } = vi.hoisted(() => ({
  check: vi.fn(),
  downloadAndInstall: vi.fn(),
  relaunch: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-updater", () => ({ check }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch }));

beforeEach(() => {
  vi.resetAllMocks();
  check.mockResolvedValue({ version: "0.5.2", downloadAndInstall });
  downloadAndInstall.mockResolvedValue(undefined);
  relaunch.mockResolvedValue(undefined);
});

describe("UpdateBanner", () => {
  it("offers an update without installing until the user consents", async () => {
    render(<UpdateBanner />);
    expect(await screen.findByText("Update available — v0.5.2")).toBeInTheDocument();
    expect(downloadAndInstall).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Install & restart" }));
    await waitFor(() => expect(relaunch).toHaveBeenCalledOnce());
    expect(downloadAndInstall).toHaveBeenCalledOnce();
  });

  it("waits for installation before relaunching and prevents duplicate installs", async () => {
    let complete!: () => void;
    downloadAndInstall.mockReturnValue(new Promise<void>((resolve) => { complete = resolve; }));
    render(<UpdateBanner />);
    fireEvent.click(await screen.findByRole("button", { name: "Install & restart" }));
    const installing = screen.getByRole("button", { name: "Installing..." });
    expect(installing).toBeDisabled();
    fireEvent.click(installing);
    expect(downloadAndInstall).toHaveBeenCalledOnce();
    expect(relaunch).not.toHaveBeenCalled();
    await act(async () => complete());
    expect(relaunch).toHaveBeenCalledOnce();
  });

  it("shows install failures without relaunching", async () => {
    downloadAndInstall.mockRejectedValue(new Error("Invalid signature"));
    render(<UpdateBanner />);
    fireEvent.click(await screen.findByRole("button", { name: "Install & restart" }));
    expect(await screen.findByText(/Update failed:.*Invalid signature/)).toBeInTheDocument();
    expect(relaunch).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByText(/Update failed/)).not.toBeInTheDocument();
  });

  it.each(["current", "offline"])("stays unobtrusive when %s", async (state) => {
    if (state === "offline") check.mockRejectedValue(new Error("offline"));
    else check.mockResolvedValue(null);
    const { container } = render(<UpdateBanner />);
    await act(async () => {});
    expect(check).toHaveBeenCalledOnce();
    expect(container).toBeEmptyDOMElement();
    expect(downloadAndInstall).not.toHaveBeenCalled();
  });
});
