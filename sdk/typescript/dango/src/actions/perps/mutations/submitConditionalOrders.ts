import { getAppConfig } from "#actions/app/queries/getAppConfig.js";
import { execute } from "#actions/app/mutations/execute.js";

import type { Address } from "@left-curve/types";
import type { SignAndBroadcastTxReturnType } from "#actions/app/mutations/signAndBroadcastTx.js";
import type { Client, Signer, TriggerDirection } from "@left-curve/types";

export type SubmitConditionalOrderInput = {
  pairId: string;
  size?: string;
  triggerPrice: string;
  triggerDirection: TriggerDirection;
  maxSlippage: string;
};

export type SubmitConditionalOrdersParameters = {
  sender: Address;
  orders: SubmitConditionalOrderInput[];
};

export type SubmitConditionalOrdersReturnType = SignAndBroadcastTxReturnType;

export async function submitConditionalOrders(
  client: Client<Signer>,
  parameters: SubmitConditionalOrdersParameters,
): SubmitConditionalOrdersReturnType {
  const { sender, orders } = parameters;

  if (orders.length === 0) {
    throw new Error("submitConditionalOrders requires at least one order");
  }

  const { addresses } = await getAppConfig(client);

  const executeMsgs = orders.map((order) => {
    const { pairId, size, triggerPrice, triggerDirection, maxSlippage } = order;

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

    return {
      msg,
      contract: addresses.perps,
    };
  });

  return await execute(client, {
    sender,
    execute: executeMsgs,
  });
}
