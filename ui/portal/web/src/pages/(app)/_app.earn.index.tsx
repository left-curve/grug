import { createFileRoute, redirect } from "@tanstack/react-router";
import { z } from "zod";

import { m } from "@left-curve/foundation/paraglide/messages.js";
import { EXCHANGE_SHUTDOWN_REDIRECT } from "~/constants";

export const Route = createFileRoute("/(app)/_app/earn/")({
  head: () => ({
    meta: [{ title: `Dango | ${m["vaultLiquidity.title"]()}` }],
  }),
  beforeLoad: async () => {
    throw redirect(EXCHANGE_SHUTDOWN_REDIRECT);
  },
  validateSearch: z.object({
    action: z.enum(["deposit", "withdraw"]).catch("deposit"),
  }),
});
