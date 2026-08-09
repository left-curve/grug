import { getAppConfig } from "#actions/app/queries/getAppConfig.js";
import { execute } from "#actions/app/mutations/execute.js";

import type { Address, Client, Signer } from "@left-curve/types";
import type { SignAndBroadcastTxReturnType } from "#actions/app/mutations/signAndBroadcastTx.js";

export type WithdrawMarginParameters = {
  sender: Address;
  amount: string;
};

export type WithdrawMarginReturnType = SignAndBroadcastTxReturnType;

export async function withdrawMargin(
  client: Client<Signer>,
  parameters: WithdrawMarginParameters,
): WithdrawMarginReturnType {
  const { sender, amount } = parameters;

  const { addresses } = await getAppConfig(client);

  const msg = {
    trade: {
      withdraw: {
        amount,
      },
    },
  };

  return await execute(client, {
    sender,
    execute: {
      msg,
      contract: addresses.perps,
    },
  });
}
