import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import Settings from "./Settings";
import { defaultAppSettings } from "../types/settings";

const invokeMock = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: () => Promise.resolve(() => {}),
}));

describe("Settings", () => {
  it("renders core sections and updates movable toggle", async () => {
    invokeMock.mockImplementation((command: string) => {
      if (command === "has_openai_api_key") {
        return Promise.resolve(true);
      }
      if (command === "list_installed_models") {
        return Promise.resolve([]);
      }
      if (command === "get_accessibility_help_info") {
        return Promise.resolve({ executable_path: "", is_dev_build: true, note: "" });
      }
      return Promise.resolve({
        settings: {
          ...defaultAppSettings,
          general: { ...defaultAppSettings.general, window_movable: false },
        },
        warnings: [],
      });
    });

    const onSettingsChange = vi.fn();
    render(<Settings settings={defaultAppSettings} onSettingsChange={onSettingsChange} />);

    expect(screen.getByText("General")).toBeInTheDocument();
    expect(screen.getByText("Setup Steps")).toBeInTheDocument();
    expect(screen.getByText("Accessibility")).toBeInTheDocument();
    expect(screen.getByText("Shortcut Step")).toBeInTheDocument();
    expect(screen.getByText("Window position")).toBeInTheDocument();
    expect(screen.getByText("Shortcuts")).toBeInTheDocument();
    expect(screen.getByText("Microphone")).toBeInTheDocument();
    expect(screen.getByText("Language")).toBeInTheDocument();
    expect(screen.getByText("Sound Settings")).toBeInTheDocument();
    expect(screen.getByText("Extras")).toBeInTheDocument();
    expect(screen.getAllByText("API").length).toBeGreaterThan(0);
    expect(screen.getByText("Transcription")).toBeInTheDocument();
    expect(screen.getByText("Save API Key")).toBeInTheDocument();
    expect(screen.getByText("Dangerously skip permissions")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("switch", { name: "Window movable" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "update_app_settings",
        expect.objectContaining({
          settings: expect.objectContaining({
            general: expect.objectContaining({
              window_movable: false,
            }),
          }),
        })
      );
      expect(onSettingsChange).toHaveBeenCalled();
    });
  });
});
