import { createFileRoute, redirect } from "@tanstack/react-router";

import { z } from "zod";
import { m } from "@left-curve/foundation/paraglide/messages.js";
import { EXCHANGE_SHUTDOWN_REDIRECT } from "~/constants";

export const Route = createFileRoute("/(app)/_app/trade/$ticker")({
  head: () => ({
    meta: [{ title: `Dango | ${m["applets.trade.title"]()}` }],
  }),
  beforeLoad: async () => {
    throw redirect(EXCHANGE_SHUTDOWN_REDIRECT);
  },
  validateSearch: z.object({
    order_type: z.enum(["limit", "market"]).default("market"),
    action: z.enum(["buy", "sell"]).default("buy"),
  }),
});
