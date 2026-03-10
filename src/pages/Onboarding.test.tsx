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
          has_openai_api_key: false,
          history: [],
        });
      }
      return Promise.resolve(null);
    });
  });

  it("validates license key before continuing", async () => {
    render(<Onboarding onComplete={vi.fn()} />);
    expect(await screen.findByText("Enter your license key")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    expect(screen.getByText("Please enter a valid license key.")).toBeInTheDocument();
    expect(screen.getByText("Step 1 of 4")).toBeInTheDocument();
  });

  it("moves to api key step for valid license", async () => {
    render(<Onboarding onComplete={vi.fn()} />);
    expect(await screen.findByText("Enter your license key")).toBeInTheDocument();

    fireEvent.change(screen.getByPlaceholderText("License key"), {
      target: { value: "LICENSE-1234" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    await waitFor(() => {
      expect(screen.getByText("Enter your OpenAI API key")).toBeInTheDocument();
    });
    expect(screen.getByText("Step 2 of 4")).toBeInTheDocument();
  });

  it("validates OpenAI key format", async () => {
    render(<Onboarding onComplete={vi.fn()} />);
    expect(await screen.findByText("Enter your license key")).toBeInTheDocument();

    fireEvent.change(screen.getByPlaceholderText("License key"), {
      target: { value: "LICENSE-1234" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    await waitFor(() => {
      expect(screen.getByText("Enter your OpenAI API key")).toBeInTheDocument();
    });

    fireEvent.change(screen.getByPlaceholderText("sk-..."), {
      target: { value: "not-a-real-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save API Key" }));

    expect(screen.getByText("Please enter a valid OpenAI API key.")).toBeInTheDocument();
  });

  it("allows continuing past accessibility step", async () => {
    render(<Onboarding onComplete={vi.fn()} />);
    expect(await screen.findByText("Enter your license key")).toBeInTheDocument();

    fireEvent.change(screen.getByPlaceholderText("License key"), {
      target: { value: "LICENSE-1234" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    await waitFor(() => {
      expect(screen.getByText("Enter your OpenAI API key")).toBeInTheDocument();
    });

    fireEvent.change(screen.getByPlaceholderText("sk-..."), {
      target: { value: "sk-test-1234" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save API Key" }));

    await waitFor(() => {
      expect(
        screen.getByText("Grant Accessibility permission so the app can type text")
      ).toBeInTheDocument();
    });

    fireEvent.click(screen.getByRole("button", { name: "Continue Anyway" }));

    await waitFor(() => {
      expect(screen.getByText("Click test, then focus any text field. The app will paste a test sentence.")).toBeInTheDocument();
    });
  });

  it("sends camelCase args when completing setup without test", async () => {
    const onComplete = vi.fn();
    render(<Onboarding onComplete={onComplete} />);
    expect(await screen.findByText("Enter your license key")).toBeInTheDocument();

    fireEvent.change(screen.getByPlaceholderText("License key"), {
      target: { value: "LICENSE-1234" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Continue" }));

    await waitFor(() => {
      expect(screen.getByText("Enter your OpenAI API key")).toBeInTheDocument();
    });

    fireEvent.change(screen.getByPlaceholderText("sk-..."), {
      target: { value: "sk-test-1234" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save API Key" }));

    await waitFor(() => {
      expect(
        screen.getByText("Grant Accessibility permission so the app can type text")
      ).toBeInTheDocument();
    });

    fireEvent.click(screen.getByRole("button", { name: "Continue Anyway" }));

    await waitFor(() => {
      expect(screen.getByText("Complete Setup Without Test")).toBeInTheDocument();
    });
    fireEvent.click(screen.getByRole("button", { name: "Complete Setup Without Test" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "save_onboarding_state",
        expect.objectContaining({
          licenseKey: "LICENSE-1234",
          onboardingComplete: true,
        })
      );
      expect(onComplete).toHaveBeenCalled();
    });
  });
});
