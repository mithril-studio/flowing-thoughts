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
      if (command === "get_persisted_state") {
        return Promise.resolve({
          groq_api_key_configured: true,
          openai_api_key_configured: false,
          active_provider: "groq",
        });
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
    expect(screen.getByText("Transcription")).toBeInTheDocument();
    expect(screen.getByText("On-device")).toBeInTheDocument();
    expect(screen.getByText("Language")).toBeInTheDocument();
    expect(screen.getByText("Shortcut")).toBeInTheDocument();
    expect(screen.getByText("Extras")).toBeInTheDocument();
    expect(screen.getByText("Permissions")).toBeInTheDocument();
    expect(screen.getByText("Cloud API (optional)")).toBeInTheDocument();
    expect(screen.getByText("Window position")).toBeInTheDocument();
    expect(screen.getByText("Save Groq API Key")).toBeInTheDocument();
    expect(screen.getByText("Active provider")).toBeInTheDocument();
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

  it("switches active provider via set_active_provider", async () => {
    invokeMock.mockReset();
    invokeMock.mockImplementation((command: string) => {
      if (command === "get_persisted_state") {
        return Promise.resolve({
          groq_api_key_configured: true,
          openai_api_key_configured: true,
          active_provider: "groq",
        });
      }
      if (command === "list_installed_models") {
        return Promise.resolve([]);
      }
      if (command === "get_accessibility_help_info") {
        return Promise.resolve({ executable_path: "", is_dev_build: true, note: "" });
      }
      if (command === "set_active_provider") {
        return Promise.resolve(null);
      }
      return Promise.resolve({ settings: defaultAppSettings, warnings: [] });
    });

    render(<Settings settings={defaultAppSettings} onSettingsChange={vi.fn()} />);

    const openaiRadio = await screen.findByRole("radio", { name: /OpenAI/i });
    fireEvent.click(openaiRadio);

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_active_provider", { provider: "openai" });
    });
  });

  it("selects an installed local model as active", async () => {
    invokeMock.mockReset();
    invokeMock.mockImplementation((command: string) => {
      if (command === "get_persisted_state") {
        return Promise.resolve({
          groq_api_key_configured: false,
          openai_api_key_configured: false,
          active_provider: "groq",
        });
      }
      if (command === "list_installed_models") {
        return Promise.resolve([
          {
            id: "whisper-base-q5",
            display_name: "Whisper Base (English + Dutch)",
            description: "Fast and tiny.",
            filename: "ggml-base-q5_1.bin",
            installed: true,
            expected_size_bytes: 59_707_625,
            local_path: "/tmp/ggml-base-q5_1.bin",
            multilingual: true,
          },
        ]);
      }
      if (command === "get_accessibility_help_info") {
        return Promise.resolve({ executable_path: "", is_dev_build: true, note: "" });
      }
      return Promise.resolve({ settings: defaultAppSettings, warnings: [] });
    });

    render(<Settings settings={defaultAppSettings} onSettingsChange={vi.fn()} />);

    const useButton = await screen.findByRole("button", { name: "Use" });
    fireEvent.click(useButton);

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "update_app_settings",
        expect.objectContaining({
          settings: expect.objectContaining({
            transcription: expect.objectContaining({
              provider: "local",
              local_model: "whisper-base-q5",
            }),
          }),
        })
      );
    });
  });
});
