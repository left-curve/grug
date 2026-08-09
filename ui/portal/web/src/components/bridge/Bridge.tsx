import {
  useAccount,
  useBalances,
  useBridgeState,
  useBridgeWithdraw,
  usePrices,
} from "@left-curve/store";

import {
  AssetInputWithRange,
  createContext,
  FormattedNumber,
  IconDisconnect,
  Modals,
  useApp,
  useInputs,
} from "@left-curve/applets-kit";

import { Link } from "@tanstack/react-router";
import { m } from "@left-curve/foundation/paraglide/messages.js";
import { Decimal, formatUnits } from "@left-curve/utils";
import { Image } from "~/components/foundation/Image";

import {
  Button,
  CoinSelector,
  IconArrowDown,
  Input,
  NetworkSelector,
  ResizerContainer,
  Tabs,
  WarningContainer,
} from "@left-curve/applets-kit";

import { useMemo, useState } from "react";
import type React from "react";
import type { PropsWithChildren } from "react";
import type { AnyCoin } from "@left-curve/store/types";
import { SwapperDeposit } from "./SwapperDeposit";

const [BridgeProvider, useBridge] = createContext<{
  state: ReturnType<typeof useBridgeState>;
  controllers: ReturnType<typeof useInputs>;
}>({
  name: "BridgeContext",
});

type BridgeProps = {
  action: "deposit" | "withdraw";
  changeAction: (action: "deposit" | "withdraw") => void;
};

const compactTokenAmountFormatter = new Intl.NumberFormat("en", {
  maximumFractionDigits: 3,
  notation: "compact",
});

const formatCompactTokenAmount = (amount: string) => {
  const numericAmount = Number(amount);
  if (!Number.isFinite(numericAmount)) return amount;
  return compactTokenAmountFormatter.format(numericAmount);
};

const formatWithdrawLiquidity = (rawBalance: string, coin: AnyCoin) => {
  const amount = formatUnits(rawBalance, coin.decimals);
  return `${m["bridge.withdrawLiquidity"]()}: ${formatCompactTokenAmount(amount)} ${coin.symbol}`;
};

const BridgeContainer: React.FC<PropsWithChildren<BridgeProps>> = ({
  children,
  action,
  changeAction,
}) => {
  const controllers = useInputs();
  const state = useBridgeState({ action, controllers, config: import.meta.env.HYPERLANE_CONFIG });

  return (
    <BridgeProvider value={{ state, controllers }}>
      <ResizerContainer
        layoutId="send-and-receive"
        className="max-w-[400px] flex flex-col gap-8 rounded-xl w-full"
      >
        <Tabs
          layoutId="tabs-send-and-receive"
          selectedTab={action}
          keys={["deposit", "withdraw"]}
          fullWidth
          onTabChange={() => changeAction(action === "deposit" ? "withdraw" : "deposit")}
        />
        {children}
      </ResizerContainer>
    </BridgeProvider>
  );
};

const BridgeDeposit: React.FC = () => {
  const { state } = useBridge();
  const { action, connector } = state;
  const { isConnected } = useAccount();
  const { showModal } = useApp();
  const handleSignin = () => showModal(Modals.Authenticate, { action: "signin" });

  if (action !== "deposit") return null;

  if (!isConnected) {
    return (
      <>
        <WarningContainer description={m["bridge.rateLimitWarning"]()} />
        <Button fullWidth onClick={handleSignin}>
          {m["common.signin"]()}
        </Button>
      </>
    );
  }

  return (
    <>
      <WarningContainer description={m["bridge.rateLimitWarning"]()} />
      <SwapperDeposit signerConnector={connector} />
    </>
  );
};

const BridgeSelectors: React.FC<{ showCoinSelector?: boolean }> = ({ showCoinSelector = true }) => {
  const { account, isConnected } = useAccount();
  const { data: balances = {} } = useBalances({ address: account?.address });

  const { state } = useBridge();
  const { coin, changeCoin, coins, network, setNetwork, networks } = state;
  const withdrawBalance = coin ? (balances[coin.denom] ?? "0") : undefined;
  const networkOptions = useMemo(() => {
    if (state.action !== "withdraw" || !coin || withdrawBalance === undefined) return networks;

    const withdrawLiquidity = formatWithdrawLiquidity(withdrawBalance, coin);
    return networks.map((network) => ({
      ...network,
      withdrawLiquidity,
    }));
  }, [coin, networks, state.action, withdrawBalance]);

  return (
    <>
      {showCoinSelector && (
        <CoinSelector
          isDisabled={!isConnected}
          label={m["bridge.selectCoin"]()}
          placeholder={m["bridge.selectCoin"]()}
          variant="boxed"
          classNames={{ base: "w-full", trigger: "h-[56px]", listboxWrapper: "top-[4rem]" }}
          value={coin?.denom}
          onChange={changeCoin}
          coins={coins}
          withName
          withPrice
        />
      )}

      <NetworkSelector
        isDisabled={!coin}
        value={network ? network : undefined}
        classNames={{ trigger: "h-[56px]" }}
        label={m["bridge.selectNetwork"]()}
        placeholder={m["bridge.selectNetwork"]()}
        onNetworkChange={({ id }) => setNetwork(id)}
        networks={networkOptions}
      />
    </>
  );
};

const BridgeWithdraw: React.FC = () => {
  const { account, isConnected } = useAccount();
  const { state, controllers } = useBridge();
  const { data: balances = {} } = useBalances({ address: account?.address });
  const { getPrice } = usePrices();
  const { action, coin, network, config, reset } = state;
  const { inputs } = controllers;
  const { showModal } = useApp();

  const [destinationAddress, setDestinationAddress] = useState<{
    address: string;
    walletName?: string;
    walletIcon?: string;
  } | null>(null);

  const amount = inputs.amount?.value || "0";
  const recipient = inputs.recipient?.value || "";

  const { withdraw, withdrawFee } = useBridgeWithdraw({
    coin: coin as AnyCoin,
    config: config as NonNullable<typeof config>,
    amount,
    recipient,
    reset,
  });

  const fee = withdrawFee.data || "0";

  const handleWithdraw = () =>
    showModal(Modals.BridgeWithdraw, {
      coin,
      config,
      amount,
      recipient,
      withdraw,
      fee,
    });

  const handleSignin = () => showModal(Modals.Authenticate, { action: "signin" });

  const handleAddressSet = (address: string, walletName?: string, walletIcon?: string) => {
    setDestinationAddress({ address, walletName, walletIcon });
    controllers.setValue("recipient", address);
  };

  const handleDisconnect = () => {
    setDestinationAddress(null);
    controllers.setValue("recipient", "");
  };

  const feeSubtraction = Decimal(amount).minus(fee);
  const youGet = feeSubtraction.gt("0") ? feeSubtraction.toFixed() : "0";

  if (action !== "withdraw") return null;

  const withdrawHintParts = m["bridge.withdrawTransferHint"]({ app: "{app}" }).split("{app}");
  const withdrawWarning = (
    <ul className="list-disc pl-4 flex flex-col gap-1">
      <li>
        {withdrawHintParts[0]}
        <Button as={Link} to="/transfer" variant="link" size="xs" className="p-0 h-fit m-0 inline">
          {m["sendAndReceive.title"]()}
        </Button>
        {withdrawHintParts[1]}
      </li>
      <li>{m["bridge.rateLimitWarning"]()}</li>
    </ul>
  );

  if (!isConnected) {
    return (
      <>
        <WarningContainer description={withdrawWarning} />
        <Button fullWidth onClick={handleSignin}>
          {m["common.signin"]()}
        </Button>
      </>
    );
  }

  return (
    <>
      <WarningContainer description={withdrawWarning} />

      <BridgeSelectors />

      {coin && network && (
        <div className="flex flex-col items-center justify-center gap-6">
          <div className="flex flex-col items-center gap-4 w-full">
            {!destinationAddress ? (
              <Button
                variant="primary"
                fullWidth
                onClick={() =>
                  showModal(Modals.DestinationWallet, {
                    onAddressSet: handleAddressSet,
                  })
                }
              >
                {m["bridge.setDestinationAddress"]()}
              </Button>
            ) : (
              <div className="flex flex-col gap-2 w-full">
                <div className="flex justify-between items-center w-full">
                  <p className="exposure-sm-italic text-ink-secondary-700">
                    {m["bridge.withdrawAddress"]()}
                  </p>
                  <div className="flex gap-2 items-center">
                    {destinationAddress.walletName && (
                      <>
                        {destinationAddress.walletIcon && (
                          <Image
                            src={destinationAddress.walletIcon}
                            alt={destinationAddress.walletName}
                            className="w-4 h-4 inline-block"
                          />
                        )}
                        <span className="diatype-sm-medium text-ink-tertiary-500">
                          {destinationAddress.walletName}
                        </span>
                      </>
                    )}
                    <IconDisconnect
                      className="w-4 h-4 inline-block text-ink-tertiary-500 hover:cursor-pointer hover:text-ink-primary-900"
                      onClick={handleDisconnect}
                    />
                  </div>
                </div>
                <div className="diatype-sm-regular text-ink-primary-900 break-all bg-surface-secondary-rice shadow-account-card rounded-lg p-3">
                  {destinationAddress.address}
                </div>
              </div>
            )}
          </div>

          {destinationAddress && (
            <>
              <AssetInputWithRange
                name="amount"
                asset={coin}
                balances={balances}
                controllers={controllers}
                showRange
                label={m["bridge.youWithdraw"]()}
                bottomComponent={
                  <div className="w-full flex justify-between">
                    <p>{m["bridge.minimumWithdraw"]()}</p>
                    <p className="flex gap-1">
                      <span>{`> ${fee}`}</span>
                      <span>{coin.symbol}</span>
                    </p>
                  </div>
                }
              />
              <div className="flex items-center justify-center">
                <div className="flex items-center justify-center border border-fg-tertiary-400 rounded-full h-5 w-5">
                  <IconArrowDown className="h-3 w-3 text-ink-tertiary-500" />
                </div>
              </div>
              <Input
                placeholder="0"
                readOnly
                label={m["bridge.youGet"]()}
                value={youGet}
                classNames={{
                  base: "z-20",
                  inputWrapper:
                    "pl-0 py-3 flex-col h-auto gap-[6px] hover:bg-surface-secondary-rice",
                  inputParent: "h-[34px] h3-bold",
                  input: "!h3-bold",
                }}
                startText="right"
                startContent={
                  <div className="inline-flex flex-row items-center gap-3 diatype-m-regular h-[46px] rounded-md min-w-14 p-3 bg-transparent justify-start">
                    <div className="flex gap-2 items-center font-semibold">
                      <Image src={coin.logoURI} alt={coin.symbol} className="w-8 h-8" />
                      <p>{coin.symbol}</p>
                    </div>
                  </div>
                }
                bottomComponent={
                  <div className="w-full flex justify-between">
                    <p>{m["bridge.fee"]()}</p>
                    <p className="flex gap-1">
                      <span>{fee}</span>
                      <span>{coin.symbol}</span>
                    </p>
                  </div>
                }
                insideBottomComponent={
                  <div className="flex justify-end w-full h-[22px] text-ink-tertiary-500 diatype-sm-regular">
                    <FormattedNumber
                      number={getPrice(youGet, coin.denom)}
                      formatOptions={{ currency: "USD" }}
                      as="p"
                    />
                  </div>
                }
              />

              <Button fullWidth onClick={handleWithdraw} className="mt-4" isDisabled={!recipient}>
                {m["bridge.withdraw.title"]()}
              </Button>
            </>
          )}
        </div>
      )}
    </>
  );
};

export const Bridge = Object.assign(BridgeContainer, {
  Deposit: BridgeDeposit,
  Withdraw: BridgeWithdraw,
});
