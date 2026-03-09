import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import RecordingIndicator from "./RecordingIndicator";

describe("RecordingIndicator", () => {
  it("shows idle helper text", () => {
    render(<RecordingIndicator mode="idle" />);
    expect(screen.getByText("Hold Cmd+Shift+Space")).toBeInTheDocument();
  });

  it("shows recording text", () => {
    render(<RecordingIndicator mode="recording" />);
    expect(screen.getByText("Recording...")).toBeInTheDocument();
  });

  it("shows transcribing text", () => {
    render(<RecordingIndicator mode="transcribing" />);
    expect(screen.getByText("Transcribing...")).toBeInTheDocument();
  });

  it("shows injecting text", () => {
    render(<RecordingIndicator mode="injecting" />);
    expect(screen.getByText("Typing...")).toBeInTheDocument();
  });

  it("shows error text", () => {
    render(<RecordingIndicator mode="error" />);
    expect(screen.getByText("Pipeline error")).toBeInTheDocument();
  });
});
