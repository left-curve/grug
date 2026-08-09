import { forwardRef, useImperativeHandle } from "react";

import { IconButton, IconClose, useApp } from "@left-curve/applets-kit";
import { m } from "@left-curve/foundation/paraglide/messages.js";

import type { ModalRef } from "./RootModal";

const ANNOUNCEMENT_URL = "https://x.com/dango/status/2080707796144705625";

export const WindingDown = forwardRef<ModalRef>((_, ref) => {
  const hideModal = useApp((state) => state.hideModal);

  useImperativeHandle(ref, () => ({
    triggerOnClose: () => {},
  }));

  return (
    <div className="relative flex max-w-[400px] flex-col gap-4 rounded-xl bg-surface-primary-rice p-6 md:border md:border-outline-secondary-gray">
      <IconButton
        aria-label={m["common.dismiss"]()}
        className="absolute right-3 top-3"
        variant="link"
        onClick={hideModal}
      >
        <IconClose className="h-5 w-5 text-ink-tertiary-500" />
      </IconButton>
      <div className="flex flex-col gap-2 pr-8">
        <h2 className="diatype-lg-bold text-ink-primary-900">{m["announcement.title"]()}</h2>
        <p className="diatype-m-regular text-ink-tertiary-500">
          {m["announcement.windingDown"]()}{" "}
          <a
            className="diatype-m-bold text-ink-secondary-blue underline underline-offset-[3px]"
            href={ANNOUNCEMENT_URL}
            target="_blank"
            rel="noopener noreferrer"
          >
            {m["announcement.readMore"]()}
          </a>
          .
        </p>
      </div>
    </div>
  );
});
