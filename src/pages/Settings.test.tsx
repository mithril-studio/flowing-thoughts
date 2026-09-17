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
    expect(screen.getByText("Window position")).toBeInTheDocument();
    expect(screen.getByText("Theme")).toBeInTheDocument();
    expect(screen.getByText("Dangerously skip permissions")).toBeInTheDocument();
    // Local provider is the default, so local models are visible and the
    // cloud key form is hidden.
    expect(screen.getByText("Local models")).toBeInTheDocument();
    expect(screen.queryByText("Save Groq API Key")).not.toBeInTheDocument();

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

    render(
      <Settings
        settings={{
          ...defaultAppSettings,
          transcription: { ...defaultAppSettings.transcription, provider: "api" },
        }}
        onSettingsChange={vi.fn()}
      />
    );

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

  it("adds a custom whisper model by name", async () => {
    invokeMock.mockImplementation((command: string) => {
      if (command === "get_persisted_state") {
        return Promise.resolve({
          groq_api_key_configured: false,
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
      if (command === "add_custom_model") {
        return Promise.resolve("ggml-medium-q5_0");
      }
      return Promise.resolve({ settings: defaultAppSettings, warnings: [] });
    });

    render(<Settings settings={defaultAppSettings} onSettingsChange={vi.fn()} />);

    const input = screen.getByLabelText("Add a whisper.cpp model");
    fireEvent.change(input, { target: { value: " medium-q5_0 " } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("add_custom_model", { source: "medium-q5_0" });
    });
    expect(await screen.findByText("Downloading ggml-medium-q5_0…")).toBeInTheDocument();
  });

  it("removes an undownloaded catalog model from the list and can restore it", async () => {
    let hidden = false;
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
            installed: false,
            expected_size_bytes: 1,
            local_path: null,
            multilingual: true,
            custom: false,
            hidden,
          },
        ]);
      }
      if (command === "hide_model") {
        hidden = true;
        return Promise.resolve();
      }
      if (command === "unhide_model") {
        hidden = false;
        return Promise.resolve();
      }
      if (command === "get_accessibility_help_info") {
        return Promise.resolve({ executable_path: "", is_dev_build: true, note: "" });
      }
      return Promise.resolve({ settings: defaultAppSettings, warnings: [] });
    });

    render(<Settings settings={defaultAppSettings} onSettingsChange={vi.fn()} />);

    fireEvent.click(await screen.findByRole("button", { name: "Remove" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("hide_model", { modelId: "whisper-base-q5" });
    });
    expect(await screen.findByText("Show 1 removed model")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Download" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByText("Show 1 removed model"));
    fireEvent.click(await screen.findByRole("button", { name: "Restore" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("unhide_model", { modelId: "whisper-base-q5" });
    });
    expect(await screen.findByRole("button", { name: "Download" })).toBeInTheDocument();
  });
});
