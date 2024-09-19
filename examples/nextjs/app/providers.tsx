"use client";

import { http, createConfig, eip1193, passkey } from "@leftcurve/connect-kit";
import { localhost } from "@leftcurve/connect-kit/chains";
import { GrugProvider } from "@leftcurve/react";
import type React from "react";
import "@leftcurve/types/window";

export const config = createConfig({
  ssr: true,
  chains: [localhost],
  transports: {
    [localhost.id]: http("http://localhost:26657"),
  },
  coins: {
    [localhost.id]: {
      uusdc: {
        type: "native",
        name: "USD Circle",
        symbol: "USDC",
        denom: "uusdc",
        decimals: 6,
        coingeckoId: "usd-coin",
      },
    },
  },
  connectors: [
    eip1193({
      id: "metamask",
      name: "Metamask",
    }),
    eip1193({
      id: "keplr",
      name: "Keplr",
      provider: () => window.keplr?.ethereum,
    }),
    passkey(),
  ],
});

export interface ProvidersProps {
  children: React.ReactNode;
}

export function Providers({ children }: ProvidersProps) {
  return <GrugProvider config={config}>{children}</GrugProvider>;
}
