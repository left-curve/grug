import { IconClose, Marquee, Modals, twMerge, useApp } from "@left-curve/applets-kit";

import type React from "react";

import { useState } from "react";

import { motion } from "framer-motion";
import { m } from "@left-curve/foundation/paraglide/messages.js";

export const TestnetBanner: React.FC = () => {
  const [isVisible, setIsVisible] = useState(true);
  const showModal = useApp((state) => state.showModal);

  if (!isVisible) return null;
  const isLandingPage = location.pathname === "/";

  return (
    <motion.div
      exit={{ transform: "scaleY(0)", height: 0, opacity: 0 }}
      transition={{ duration: 0.2 }}
      className={twMerge(
        "min-h-9 h-9 w-full relative top-0 bg-[url('./images/warning-banner.svg')] flex items-center justify-center",
        isLandingPage && "relative z-50",
      )}
    >
      <button
        type="button"
        aria-label={m["announcement.title"]()}
        className="h-full w-full cursor-pointer"
        onClick={() => showModal(Modals.WindingDown)}
      >
        <Marquee
          className="w-full bg-[#F7CF74] h-fit p-0 uppercase gap-10"
          item={
            <div className="flex gap-10 items-center text-primitives-gray-light-700 diatype-sm-heavy ml-10">
              <span>{m["announcement.title"]()}</span>
              <span>•</span>
            </div>
          }
          speed={50}
        />
      </button>
      <button
        type="button"
        aria-label={m["common.dismiss"]()}
        className="absolute right-3 top-[7px] h-6 w-6 z-10 rounded-full bg-primitives-red-light-50 border border-primitives-gray-light-100 flex items-center justify-center"
        onClick={() => setIsVisible(false)}
      >
        <IconClose className="text-primitives-red-light-500 w-5 h-5" />
      </button>
    </motion.div>
  );
};
