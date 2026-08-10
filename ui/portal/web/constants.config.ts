import { m } from "@left-curve/foundation/paraglide/messages.js";

import type { AppletMetadata } from "@left-curve/store/types";

export const DEFAULT_SESSION_EXPIRATION = 24 * 60 * 60 * 1000; // 24 hours

export const PRIVY_APP_ID = import.meta.env.PUBLIC_PRIVY_APP_ID;
export const PRIVY_CLIENT_ID = import.meta.env.PUBLIC_PRIVY_CLIENT_ID;

export const PRIVY_ERRORS_MAPPING = {
  "User already has one email account linked": m["auth.errors.userNotFound"](),
  authFailed: m["auth.errors.authFailed"](),
  "User does not exist": m["auth.errors.userNotFound"](),
};

export const WS_URI = import.meta.env.PUBLIC_WS_URI;

/** Default max slippage for perps market & TP/SL orders (0.5%). */
export const PERPS_DEFAULT_SLIPPAGE = "0.005";

/** 14-day lookback window (in seconds), matching the backend VOLUME_LOOKBACK. */
export const FEE_VOLUME_LOOKBACK_SECONDS = 14 * 24 * 60 * 60;

export const EXCHANGE_SHUTDOWN_NOTICE = "exchange-shutdown" as const;

export const EXCHANGE_SHUTDOWN_REDIRECT = {
  replace: true,
  search: { notice: EXCHANGE_SHUTDOWN_NOTICE },
  to: "/",
} as const;

const DISABLED_APPLET_IDS = new Set(["earn", "trade"]);

const translations = m as unknown as Record<string, () => string>;
export const APPLETS: Record<string, AppletMetadata> = Object.keys(translations)
  .filter((k) => /^applets\..*\.id$/.test(k))
  .reduce((acc, key) => {
    const [_, id] = key.split(".");
    acc[id] = {
      id,
      title: translations[`applets.${id}.title`](),
      description: translations[`applets.${id}.description`](),
      img: translations[`applets.${id}.img`](),
      keywords: translations[`applets.${id}.keywords`]().split(","),
      path: translations[`applets.${id}.path`](),
      isDisabled: DISABLED_APPLET_IDS.has(id),
    };
    return acc;
  }, Object.create({}));
