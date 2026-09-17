import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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

  it("keeps dictation audio for evaluation only after an explicit opt-in", async () => {
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
        return Promise.resolve([]);
      }
      if (command === "get_accessibility_help_info") {
        return Promise.resolve({ executable_path: "", is_dev_build: true, note: "" });
      }
      return Promise.resolve({ settings: defaultAppSettings, warnings: [] });
    });

    expect(defaultAppSettings.extras.keep_audio_for_eval).toBe(false);
    render(<Settings settings={defaultAppSettings} onSettingsChange={vi.fn()} />);

    const toggle = screen.getByRole("switch", { name: "Keep my dictations for evaluation" });
    expect(toggle).toHaveAttribute("aria-checked", "false");
    fireEvent.click(toggle);

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "update_app_settings",
        expect.objectContaining({
          settings: expect.objectContaining({
            extras: expect.objectContaining({ keep_audio_for_eval: true }),
          }),
        })
      );
    });
  });

  describe("Meetings section", () => {
    const meetingsOn = {
      ...defaultAppSettings,
      meetings: { ...defaultAppSettings.meetings, enabled: true },
    };

    /** Echoes saved settings back, like the backend does. */
    function mockMeetingsBackend(overrides: Record<string, unknown> = {}) {
      invokeMock.mockReset();
      invokeMock.mockImplementation((command: string, args?: { settings?: unknown }) => {
        if (command in overrides) return Promise.resolve(overrides[command]);
        switch (command) {
          case "get_persisted_state":
            return Promise.resolve({
              groq_api_key_configured: false,
              openai_api_key_configured: false,
              openrouter_api_key_configured: false,
              active_provider: "groq",
            });
          case "list_installed_models":
            return Promise.resolve([]);
          case "get_accessibility_help_info":
            return Promise.resolve({ executable_path: "", is_dev_build: true, note: "" });
          case "meetings_supported":
            return Promise.resolve(true);
          case "check_system_audio_permission":
            return Promise.resolve({ state: "unknown", detail: null });
          case "update_app_settings":
            return Promise.resolve({ settings: args?.settings, warnings: [] });
          default:
            return Promise.resolve(null);
        }
      });
    }

    it("persists the enable toggle through update_app_settings", async () => {
      mockMeetingsBackend();
      const onSettingsChange = vi.fn();
      render(<Settings settings={defaultAppSettings} onSettingsChange={onSettingsChange} />);

      const toggle = screen.getByRole("switch", { name: "Meeting recording" });
      expect(toggle).toHaveAttribute("aria-checked", "false");
      // Off by default: no meeting options and no permission probe.
      expect(screen.queryByRole("group", { name: "Meeting language" })).not.toBeInTheDocument();
      expect(invokeMock).not.toHaveBeenCalledWith("check_system_audio_permission");

      fireEvent.click(toggle);

      await waitFor(() => {
        expect(invokeMock).toHaveBeenCalledWith(
          "update_app_settings",
          expect.objectContaining({
            settings: expect.objectContaining({
              meetings: expect.objectContaining({ enabled: true }),
            }),
          }),
        );
        expect(onSettingsChange).toHaveBeenCalledWith(
          expect.objectContaining({ meetings: expect.objectContaining({ enabled: true }) }),
        );
      });
      expect(await screen.findByRole("group", { name: "Meeting language" })).toBeInTheDocument();
    });

    it("saves the meeting language and an installed Whisper model", async () => {
      const model = (id: string, display_name: string) => ({
        id,
        display_name,
        description: "",
        filename: `${id}.bin`,
        installed: true,
        expected_size_bytes: 1,
        local_path: `/tmp/${id}.bin`,
        multilingual: true,
        custom: false,
        hidden: false,
      });
      mockMeetingsBackend({
        list_installed_models: [
          model("whisper-small-q5", "Whisper Small"),
          model("whisper-large-v3-turbo-q5", "Whisper Large Turbo"),
          model("parakeet-tdt-0.6b-v3", "Parakeet"),
        ],
      });
      render(<Settings settings={meetingsOn} onSettingsChange={vi.fn()} />);

      const language = screen.getByRole("group", { name: "Meeting language" });
      fireEvent.click(within(language).getByRole("button", { name: "Nederlands" }));
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith(
          "update_app_settings",
          expect.objectContaining({
            settings: expect.objectContaining({
              meetings: expect.objectContaining({ language: "nl" }),
            }),
          }),
        ),
      );

      const select = screen.getByLabelText("Meeting model");
      await within(select).findByRole("option", { name: "Whisper Large Turbo" });
      // Meetings decode through Whisper only.
      expect(within(select).queryByRole("option", { name: "Parakeet" })).not.toBeInTheDocument();
      await waitFor(() => expect(select).toBeEnabled());
      fireEvent.change(select, { target: { value: "whisper-large-v3-turbo-q5" } });
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith(
          "update_app_settings",
          expect.objectContaining({
            settings: expect.objectContaining({
              meetings: expect.objectContaining({ model: "whisper-large-v3-turbo-q5" }),
            }),
          }),
        ),
      );
    });

    it("keeps summaries opt-in and saves the summary model on blur", async () => {
      mockMeetingsBackend();
      render(<Settings settings={meetingsOn} onSettingsChange={vi.fn()} />);

      const toggle = screen.getByRole("switch", { name: "Meeting summaries" });
      expect(toggle).toHaveAttribute("aria-checked", "false");
      expect(screen.queryByLabelText("Summary model")).not.toBeInTheDocument();

      fireEvent.click(toggle);
      const field = await screen.findByLabelText("Summary model");
      fireEvent.change(field, { target: { value: "google/gemini-2.5-flash-lite" } });
      fireEvent.blur(field);

      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith(
          "update_app_settings",
          expect.objectContaining({
            settings: expect.objectContaining({
              meetings: expect.objectContaining({
                summary_enabled: true,
                summary_model: "google/gemini-2.5-flash-lite",
              }),
            }),
          }),
        ),
      );
    });

    it("turns audio auto-delete on with a day count and off with zero", async () => {
      mockMeetingsBackend();
      render(<Settings settings={meetingsOn} onSettingsChange={vi.fn()} />);

      const toggle = screen.getByRole("switch", { name: "Auto-delete meeting audio" });
      expect(toggle).toHaveAttribute("aria-checked", "false");
      fireEvent.click(toggle);
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith(
          "update_app_settings",
          expect.objectContaining({
            settings: expect.objectContaining({
              meetings: expect.objectContaining({ auto_delete_audio_days: 30 }),
            }),
          }),
        ),
      );

      const days = await screen.findByRole("group", { name: "Delete audio after" });
      await waitFor(() => expect(within(days).getByRole("button", { name: "7 days" })).toBeEnabled());
      fireEvent.click(within(days).getByRole("button", { name: "7 days" }));
      await waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith(
          "update_app_settings",
          expect.objectContaining({
            settings: expect.objectContaining({
              meetings: expect.objectContaining({ auto_delete_audio_days: 7 }),
            }),
          }),
        ),
      );
    });

    it("shows the system audio permission and opens System Settings", async () => {
      mockMeetingsBackend({ check_system_audio_permission: { state: "denied", detail: null } });
      render(<Settings settings={meetingsOn} onSettingsChange={vi.fn()} />);

      const title = await screen.findByText("System Audio Recording");
      const row = title.parentElement!.parentElement!.parentElement!;
      expect(await within(row).findByText("Not granted")).toBeInTheDocument();
      fireEvent.click(within(row).getByRole("button", { name: "Open Settings" }));
      await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("open_system_audio_settings"));
    });

    it("does not call an unknown permission a denial", async () => {
      mockMeetingsBackend();
      render(<Settings settings={meetingsOn} onSettingsChange={vi.fn()} />);
      expect(await screen.findByText("Not checked yet")).toBeInTheDocument();
      expect(screen.queryByText("Not granted")).not.toBeInTheDocument();
    });

    it("explains the macOS requirement instead of the permission row", async () => {
      mockMeetingsBackend({ meetings_supported: false });
      render(<Settings settings={meetingsOn} onSettingsChange={vi.fn()} />);
      expect(await screen.findByText(/Meetings need macOS 14.4 or later/)).toBeInTheDocument();
      expect(screen.queryByText("System Audio Recording")).not.toBeInTheDocument();
    });
  });
});
