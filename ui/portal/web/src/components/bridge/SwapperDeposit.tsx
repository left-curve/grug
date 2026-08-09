import {
  type ConnectorWalletClient,
  isEvmProviderConnector,
  useAccount,
  useBalances,
  useConnectorWalletClient,
} from "@left-curve/store";

import { Button, IconChevronRight, WarningContainer, useTheme } from "@left-curve/applets-kit";

import { m } from "@left-curve/foundation/paraglide/messages.js";
import {
  SwapperIframe,
  WidgetEventName,
  type ComponentStyles,
  type SwapperStyles,
} from "@swapper-finance/deposit-sdk";

import { useCallback, useEffect, useRef } from "react";

import type { Connector } from "@left-curve/store/types";

const SWAPPER_DST_CHAIN_ID = "dango";
const SWAPPER_DST_TOKEN_ADDR = "usdc";
const SWAPPER_IFRAME_HEIGHT = "560px";
const SWAPPER_DEPOSIT_OPTIONS = [
  "transferCrypto",
  "depositWithCash",
  "walletDeposit",
  "depositFromPerps",
  "depositFromPolymarket",
] as const;
const SWAPPER_CONTAINER_CLASS =
  "h-fit w-full overflow-hidden rounded-xl bg-surface-secondary-rice shadow-account-card";
const SWAPPER_BASE_COMPONENT_STYLES = {
  width: "100%",
  primaryColor: "#F57589",
  accentColor: "#F57589",
  sphereColor: "#F57589",
  border: "0",
  borderRadius: "12px",
} satisfies ComponentStyles;

const getCssVariableColor = (variable: string) => {
  const color = getComputedStyle(document.documentElement).getPropertyValue(variable).trim();
  return color || `var(${variable})`;
};

const getSwapperStyles = (theme: "light" | "dark"): SwapperStyles => ({
  themeMode: theme,
  componentStyles: {
    ...SWAPPER_BASE_COMPONENT_STYLES,
    backgroundColor: getCssVariableColor("--color-surface-secondary-rice"),
    primaryButtonTextColor: getCssVariableColor("--color-surface-primary-rice"),
    surfaceColor: getCssVariableColor("--color-surface-tertiary-rice"),
    surfaceAltColor: getCssVariableColor("--color-surface-quaternary-rice"),
    textColor: getCssVariableColor("--color-ink-primary-900"),
  },
});

type SwapperDepositProps = {
  onBack?: () => void;
  signerConnector?: Connector;
};

type UseSwapperSignerClientParameters = {
  dangoConnector?: Connector;
  enabled: boolean;
  signerConnector?: Connector;
};

function getSwapperSignerConnector(
  signerConnector: Connector | undefined,
  dangoConnector: Connector | undefined,
) {
  return isEvmProviderConnector(signerConnector)
    ? signerConnector
    : isEvmProviderConnector(dangoConnector)
      ? dangoConnector
      : undefined;
}

function useSwapperSignerClient({
  dangoConnector,
  enabled,
  signerConnector,
}: UseSwapperSignerClientParameters) {
  const swapperSignerConnector = getSwapperSignerConnector(signerConnector, dangoConnector);
  const signerWallet = useConnectorWalletClient({
    connector: swapperSignerConnector,
    enabled,
  });

  return {
    signerClient: signerWallet.data,
    isPending: enabled && !!swapperSignerConnector && !signerWallet.data && !signerWallet.error,
  };
}

type SwapperIframeMountProps = {
  depositWalletAddress: string;
  integratorId: string;
  onTransactionSuccess: () => void;
  signerClient?: ConnectorWalletClient<undefined>;
  theme: "light" | "dark";
};

function SwapperIframeMount({
  depositWalletAddress,
  integratorId,
  onTransactionSuccess,
  signerClient,
  theme,
}: SwapperIframeMountProps) {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    container.replaceChildren();

    const swapper = new SwapperIframe({
      container,
      integratorId,
      dstChainId: SWAPPER_DST_CHAIN_ID,
      dstTokenAddr: SWAPPER_DST_TOKEN_ADDR,
      depositWalletAddress,
      ...(signerClient ? { wallet: { signer: signerClient } } : {}),
      supportedDepositOptions: [...SWAPPER_DEPOSIT_OPTIONS],
      styles: getSwapperStyles(theme),
      iframeAttributes: {
        width: "100%",
        minWidth: "0",
        height: SWAPPER_IFRAME_HEIGHT,
        borderRadius: "12px",
        allowtransparency: "true",
      },
      onEvent: (event) => {
        if (event.type !== WidgetEventName.TRANSACTION_SUCCESS) return;
        onTransactionSuccess();
      },
    });

    const iframe = swapper.getIframe();
    iframe.style.backgroundColor = "transparent";
    iframe.style.display = "block";
    iframe.setAttribute("allowtransparency", "true");
    iframe.parentElement?.style.setProperty("height", "fit-content");

    return () => {
      swapper.destroy();
      container.replaceChildren();
    };
  }, [depositWalletAddress, integratorId, onTransactionSuccess, signerClient, theme]);

  return <div ref={containerRef} className={SWAPPER_CONTAINER_CLASS} />;
}

export const SwapperDeposit = ({ onBack, signerConnector }: SwapperDepositProps) => {
  const { account, connector: dangoConnector, isConnected, refreshUserStatus } = useAccount();
  const { refetch: refreshBalances } = useBalances({ address: account?.address });
  const { theme } = useTheme();
  const swapperIntegratorId = import.meta.env.PUBLIC_SWAPPER_INTEGRATOR_ID?.trim();
  const swapperParameters =
    account?.address && isConnected && swapperIntegratorId
      ? {
          depositWalletAddress: account.address,
          integratorId: swapperIntegratorId,
        }
      : undefined;
  const { isPending: isSignerPending, signerClient } = useSwapperSignerClient({
    dangoConnector,
    enabled: !!swapperParameters,
    signerConnector,
  });
  const handleTransactionSuccess = useCallback(() => {
    refreshBalances();
    refreshUserStatus?.();
  }, [refreshBalances, refreshUserStatus]);

  return (
    <div className="flex flex-col gap-4">
      {onBack ? (
        <div className="flex items-center gap-3">
          <Button variant="link" size="sm" className="m-0 p-0" onClick={onBack}>
            <IconChevronRight className="h-4 w-4 rotate-180" />
            {m["bridge.deposit.moreOptions.back"]()}
          </Button>
        </div>
      ) : null}

      {!swapperParameters ? (
        <WarningContainer color="error" description={m["common.failedToLoad"]()} />
      ) : isSignerPending ? (
        <div className={SWAPPER_CONTAINER_CLASS} />
      ) : (
        <SwapperIframeMount
          {...swapperParameters}
          onTransactionSuccess={handleTransactionSuccess}
          signerClient={signerClient}
          theme={theme === "dark" ? "dark" : "light"}
        />
      )}
    </div>
  );
};
