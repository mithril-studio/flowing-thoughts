import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import TabBar from "./TabBar";

describe("TabBar", () => {
  it("hides the opt-in tabs by default", () => {
    render(<TabBar active="home" onTabChange={() => {}} />);
    expect(screen.getByText("Home")).toBeInTheDocument();
    expect(screen.queryByText("Meetings")).not.toBeInTheDocument();
    expect(screen.queryByText("Coach")).not.toBeInTheDocument();
  });

  it("shows Meetings only when it is enabled", () => {
    render(<TabBar active="home" onTabChange={() => {}} showMeetings />);
    expect(screen.getByText("Meetings")).toBeInTheDocument();
    expect(screen.queryByText("Coach")).not.toBeInTheDocument();
  });

  it("gates Coach independently of Meetings", () => {
    render(<TabBar active="home" onTabChange={() => {}} showCoach />);
    expect(screen.getByText("Coach")).toBeInTheDocument();
    expect(screen.queryByText("Meetings")).not.toBeInTheDocument();
  });
});
