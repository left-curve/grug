import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { m } from "@left-curve/foundation/paraglide/messages.js";

import { resetAppletsKitMocks, setAppletsKitUseApp } from "./mocks/applets-kit";

import { WindingDown } from "../src/components/modals/WindingDown";

const announcementUrl = "https://x.com/dango/status/2080707796144705625";
const hideModal = vi.fn();

describe("WindingDown", () => {
  beforeEach(() => {
    resetAppletsKitMocks();
    setAppletsKitUseApp({ hideModal });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders the shutdown notice and announcement link", () => {
    render(<WindingDown />);

    expect(screen.getByRole("heading", { name: m["announcement.title"]() })).toHaveTextContent(
      m["announcement.title"](),
    );
    expect(screen.getByText(m["announcement.windingDown"](), { exact: false })).toHaveTextContent(
      `${m["announcement.windingDown"]()} ${m["announcement.readMore"]()}.`,
    );
    expect(screen.getByRole("link", { name: m["announcement.readMore"]() })).toHaveAttribute(
      "href",
      announcementUrl,
    );
    expect(screen.getByRole("link", { name: m["announcement.readMore"]() })).toHaveAttribute(
      "target",
      "_blank",
    );
  });

  it("closes from its accessible dismiss control", () => {
    render(<WindingDown />);

    fireEvent.click(screen.getByRole("button", { name: m["common.dismiss"]() }));

    expect(hideModal).toHaveBeenCalledOnce();
  });
});
