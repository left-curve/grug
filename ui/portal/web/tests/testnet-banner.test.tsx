import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Modals } from "@left-curve/applets-kit";
import { m } from "@left-curve/foundation/paraglide/messages.js";

import { resetAppletsKitMocks, setAppletsKitUseApp } from "./mocks/applets-kit";

import { TestnetBanner } from "../src/components/foundation/TestnetBanner";

class MockResizeObserver {
  disconnect = vi.fn();
  observe = vi.fn();
  unobserve = vi.fn();
}

function setPathname(pathname: string) {
  window.history.pushState({}, "", pathname);
}

const showModal = vi.fn();

describe("TestnetBanner", () => {
  beforeEach(() => {
    resetAppletsKitMocks();
    vi.stubGlobal("ResizeObserver", MockResizeObserver);
    setPathname("/trade");
    setAppletsKitUseApp({ showModal });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    vi.unstubAllGlobals();
  });

  it("opens the winding-down modal when users click the banner", () => {
    render(<TestnetBanner />);

    expect(screen.getAllByText(m["announcement.title"]()).length).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole("button", { name: m["announcement.title"]() }));

    expect(showModal).toHaveBeenCalledWith(Modals.WindingDown);
  });

  it("lets users dismiss the banner without opening the modal", () => {
    render(<TestnetBanner />);

    fireEvent.click(screen.getByRole("button", { name: m["common.dismiss"]() }));

    expect(screen.queryByText(m["announcement.title"]())).not.toBeInTheDocument();
    expect(showModal).not.toHaveBeenCalled();
  });

  it("keeps the banner above the landing page chrome", () => {
    setPathname("/");

    const { container } = render(<TestnetBanner />);

    expect(container.firstElementChild).toHaveClass("relative", "z-50");
  });
});
