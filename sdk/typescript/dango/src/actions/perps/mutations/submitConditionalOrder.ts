import { getAppConfig } from "#actions/app/queries/getAppConfig.js";
import { execute } from "#actions/app/mutations/execute.js";

import type { Address } from "@left-curve/types";
import type { SignAndBroadcastTxReturnType } from "#actions/app/mutations/signAndBroadcastTx.js";
import type { Client, Signer, TriggerDirection } from "@left-curve/types";

export type SubmitConditionalOrderParameters = {
  sender: Address;
  pairId: string;
  size?: string;
  triggerPrice: string;
  triggerDirection: TriggerDirection;
  maxSlippage: string;
};

export type SubmitConditionalOrderReturnType = SignAndBroadcastTxReturnType;

export async function submitConditionalOrder(
  client: Client<Signer>,
  parameters: SubmitConditionalOrderParameters,
): SubmitConditionalOrderReturnType {
  const { sender, pairId, size, triggerPrice, triggerDirection, maxSlippage } = parameters;

  const { addresses } = await getAppConfig(client);

  const msg = {
    trade: {
      submitConditionalOrder: {
        pairId,
        ...(size !== undefined ? { size } : {}),
        triggerPrice,
        triggerDirection,
        maxSlippage,
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
