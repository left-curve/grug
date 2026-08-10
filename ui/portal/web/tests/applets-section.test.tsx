import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ComponentProps, PropsWithChildren } from "react";

import { resetAppletsKitMocks, setAppletsKitUseApp } from "./mocks/applets-kit";

import { APPLETS } from "../constants.config";
import { AppletsSection } from "../src/components/landing/AppletsSection";

const appletsSectionMocks = vi.hoisted(() => ({
  favApplets: ["trade", "earn", "missing-applet", "transfer"] as string[],
  setSearchBarVisibility: vi.fn(),
}));

vi.mock("@tanstack/react-router", async () => {
  const React = await import("react");

  const Link = React.forwardRef<
    HTMLAnchorElement,
    PropsWithChildren<ComponentProps<"a"> & { to: string }>
  >(({ children, to, ...props }, ref) => (
    <a href={to} ref={ref} {...props}>
      {children}
    </a>
  ));

  Link.displayName = "TestRouterLink";

  return {
    Link,
  };
});

vi.mock("@left-curve/store", () => ({
  useFavApplets: () => ({
    favApplets: appletsSectionMocks.favApplets,
  }),
}));

describe("applets section", () => {
  beforeEach(() => {
    resetAppletsKitMocks();
    appletsSectionMocks.favApplets = ["trade", "earn", "missing-applet", "transfer"];
    setAppletsKitUseApp({
      setSearchBarVisibility: appletsSectionMocks.setSearchBarVisibility,
    });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("disables shutdown applets while keeping other favorite applets navigable", () => {
    render(<AppletsSection />);

    const trade = APPLETS.trade;
    const earn = APPLETS.earn;
    const transfer = APPLETS.transfer;

    expect(trade.isDisabled).toBe(true);
    expect(earn.isDisabled).toBe(true);
    expect(transfer.isDisabled).toBe(false);
    expect(screen.getByRole("button", { name: trade.title })).toBeDisabled();
    expect(screen.getByRole("button", { name: earn.title })).toBeDisabled();
    expect(screen.queryByRole("link", { name: trade.title })).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: earn.title })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: trade.title }).querySelector("img")).toHaveClass(
      "grayscale",
    );
    expect(screen.getByText(trade.title)).toBeInTheDocument();

    expect(screen.getByRole("link", { name: transfer.title })).toHaveAttribute(
      "href",
      transfer.path,
    );
    expect(screen.getByAltText(transfer.title)).toHaveAttribute("src", transfer.img);
    expect(screen.getByText(transfer.title)).toBeInTheDocument();
    expect(screen.queryByText("missing-applet")).not.toBeInTheDocument();
  });

  it("opens the search bar from the add applet tile", () => {
    render(<AppletsSection />);

    const addButton = screen
      .getAllByRole("button")
      .find((button) => !button.hasAttribute("disabled"));
    expect(addButton).toBeDefined();
    if (!addButton) throw new Error("Expected an enabled add applet button");
    fireEvent.click(addButton);

    expect(appletsSectionMocks.setSearchBarVisibility).toHaveBeenCalledWith(true);
  });
});
