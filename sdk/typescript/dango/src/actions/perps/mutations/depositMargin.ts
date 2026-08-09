import { getAppConfig } from "#actions/app/queries/getAppConfig.js";
import { execute } from "#actions/app/mutations/execute.js";

import type { Address, Client, Signer } from "@left-curve/types";
import type { SignAndBroadcastTxReturnType } from "#actions/app/mutations/signAndBroadcastTx.js";

export type DepositMarginParameters = {
  sender: Address;
  amount: string;
};

export type DepositMarginReturnType = SignAndBroadcastTxReturnType;

export async function depositMargin(
  client: Client<Signer>,
  parameters: DepositMarginParameters,
): DepositMarginReturnType {
  const { sender, amount } = parameters;

  const { addresses } = await getAppConfig(client);

  const msg = {
    trade: {
      deposit: {},
    },
  };

  return await execute(client, {
    sender,
    execute: {
      msg,
      contract: addresses.perps,
      funds: {
        "bridge/usdc": amount,
      },
    },
  });
}
