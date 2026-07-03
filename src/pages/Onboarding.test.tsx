import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import Onboarding from "./Onboarding";
import { defaultAppSettings } from "../types/settings";

const invokeMock = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: () => Promise.resolve(() => {}),
}));

function mockBackend(overrides: { modelInstalled?: boolean } = {}) {
  invokeMock.mockImplementation((command: string) => {
    if (command === "get_persisted_state") {
      return Promise.resolve({
        onboarding_complete: false,
        settings: { extras: { dangerously_skip_permissions: false } },
      });
    }
    if (command === "list_installed_models") {
      return Promise.resolve([
        {
          id: "whisper-small-q5",
          display_name: "Whisper Small (English + Dutch)",
          installed: Boolean(overrides.modelInstalled),
          expected_size_bytes: 190_085_487,
        },
      ]);
    }
    if (command === "get_app_settings") {
      return Promise.resolve(defaultAppSettings);
    }
    if (command === "update_app_settings") {
      return Promise.resolve({ settings: defaultAppSettings, warnings: [] });
    }
    if (command === "get_accessibility_help_info") {
      return Promise.resolve({ executable_path: "", is_dev_build: true, note: "" });
    }
    return Promise.resolve(null);
  });
}

describe("Onboarding", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    mockBackend();
  });

  it("starts on the local model step with auto language selected", async () => {
    render(<Onboarding onComplete={vi.fn()} />);
    expect(
      await screen.findByText("Which language do you speak?")
    ).toBeInTheDocument();
    expect(screen.getByText("Step 1 of 3")).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Auto" })).toHaveAttribute(
      "aria-checked",
      "true"
    );
    expect(
      screen.getByRole("button", { name: /Download model/ })
    ).toBeInTheDocument();
  });

  it("starts the recommended model download", async () => {
    render(<Onboarding onComplete={vi.fn()} />);
    const download = await screen.findByRole("button", { name: /Download model/ });
    fireEvent.click(download);

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("download_model", {
        modelId: "whisper-small-q5",
      });
    });
  });

  it("continue is enabled once the model is installed and persists local settings", async () => {
    mockBackend({ modelInstalled: true });
    render(<Onboarding onComplete={vi.fn()} />);

    expect(await screen.findByText("Model installed and ready.")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("radio", { name: "Nederlands" }));
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "update_app_settings",
        expect.objectContaining({
          settings: expect.objectContaining({
            language: expect.objectContaining({ mode: "nl" }),
            transcription: expect.objectContaining({
              provider: "local",
              local_model: "whisper-small-q5",
            }),
          }),
        })
      );
      expect(
        screen.getByText(
          "Grant Accessibility permission so FlowingThoughts can paste into apps"
        )
      ).toBeInTheDocument();
    });
  });

  it("completes setup and calls save_onboarding_state", async () => {
    const onComplete = vi.fn();
    render(<Onboarding onComplete={onComplete} />);
    await screen.findByText("Which language do you speak?");

    fireEvent.click(
      screen.getByRole("button", {
        name: /Skip for now/,
      })
    );

    await waitFor(() => {
      expect(
        screen.getByText(
          "Grant Accessibility permission so FlowingThoughts can paste into apps"
        )
      ).toBeInTheDocument();
    });

    fireEvent.click(screen.getByRole("button", { name: "Continue Anyway" }));

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Skip Test and Finish" })).toBeInTheDocument();
    });
    fireEvent.click(screen.getByRole("button", { name: "Skip Test and Finish" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "save_onboarding_state",
        expect.objectContaining({
          licenseKey: null,
          onboardingComplete: true,
        })
      );
      expect(onComplete).toHaveBeenCalled();
    });
  });
});
