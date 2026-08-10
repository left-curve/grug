import { createFileRoute, redirect } from "@tanstack/react-router";
import { EXCHANGE_SHUTDOWN_REDIRECT } from "~/constants";

export const Route = createFileRoute("/(app)/_app/trade/")({
  beforeLoad: async () => {
    throw redirect(EXCHANGE_SHUTDOWN_REDIRECT);
  },
});
