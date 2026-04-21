import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import Onboarding from "./Onboarding";

const invokeMock = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

describe("Onboarding", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockImplementation((command: string) => {
      if (command === "get_persisted_state") {
        return Promise.resolve({
          onboarding_complete: false,
          license_key: null,
          groq_api_key_configured: false,
          openai_api_key_configured: false,
          active_provider: "groq",
          history: [],
        });
      }
      return Promise.resolve(null);
    });
  });

  it("starts on the API key step with Groq selected by default", async () => {
    render(<Onboarding onComplete={vi.fn()} />);
    expect(await screen.findByText("Enter your Groq API key")).toBeInTheDocument();
    expect(screen.getByText("Step 1 of 3")).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: /Groq/i })).toHaveAttribute(
      "aria-checked",
      "true"
    );
  });

  it("validates Groq key format", async () => {
    render(<Onboarding onComplete={vi.fn()} />);
    expect(await screen.findByText("Enter your Groq API key")).toBeInTheDocument();

    fireEvent.change(screen.getByPlaceholderText("gsk_..."), {
      target: { value: "not-a-real-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save API Key" }));

    expect(screen.getByText("Please enter a valid Groq API key.")).toBeInTheDocument();
  });

  it("switching to OpenAI validates against sk- prefix", async () => {
    render(<Onboarding onComplete={vi.fn()} />);
    await screen.findByText("Enter your Groq API key");

    fireEvent.click(screen.getByRole("radio", { name: /OpenAI/i }));

    await screen.findByText("Enter your OpenAI API key");
    fireEvent.change(screen.getByPlaceholderText("sk-..."), {
      target: { value: "gsk_wrong-prefix" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save API Key" }));

    expect(screen.getByText("Please enter a valid OpenAI API key.")).toBeInTheDocument();
  });

  it("sends provider=groq when saving a Groq key", async () => {
    render(<Onboarding onComplete={vi.fn()} />);
    await screen.findByText("Enter your Groq API key");

    fireEvent.change(screen.getByPlaceholderText("gsk_..."), {
      target: { value: "gsk_test_1234" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save API Key" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_api_key", {
        provider: "groq",
        key: "gsk_test_1234",
      });
    });
  });

  it("completes setup without a license and calls save_onboarding_state with null licenseKey", async () => {
    const onComplete = vi.fn();
    render(<Onboarding onComplete={onComplete} />);
    expect(await screen.findByText("Enter your Groq API key")).toBeInTheDocument();

    fireEvent.change(screen.getByPlaceholderText("gsk_..."), {
      target: { value: "gsk_test_1234" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save API Key" }));

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

  it("skips straight to accessibility when any provider key is already configured", async () => {
    invokeMock.mockReset();
    invokeMock.mockImplementation((command: string) => {
      if (command === "get_persisted_state") {
        return Promise.resolve({
          onboarding_complete: false,
          license_key: null,
          groq_api_key_configured: true,
          openai_api_key_configured: false,
          active_provider: "groq",
          history: [],
          settings: { extras: { dangerously_skip_permissions: false } },
        });
      }
      return Promise.resolve(null);
    });

    render(<Onboarding onComplete={vi.fn()} />);

    await waitFor(() => {
      expect(screen.getByText("Step 2 of 3")).toBeInTheDocument();
    });
    expect(
      screen.getByText(
        "Grant Accessibility permission so FlowingThoughts can paste into apps"
      )
    ).toBeInTheDocument();
  });
});
