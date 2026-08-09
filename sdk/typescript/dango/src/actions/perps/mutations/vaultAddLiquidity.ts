import { getAppConfig } from "#actions/app/queries/getAppConfig.js";
import { execute } from "#actions/app/mutations/execute.js";

import type { Address, Client, Signer } from "@left-curve/types";
import type { SignAndBroadcastTxReturnType } from "#actions/app/mutations/signAndBroadcastTx.js";

export type VaultAddLiquidityParameters = {
  sender: Address;
  amount: string;
  minSharesToMint?: string;
};

export type VaultAddLiquidityReturnType = SignAndBroadcastTxReturnType;

export async function vaultAddLiquidity(
  client: Client<Signer>,
  parameters: VaultAddLiquidityParameters,
): VaultAddLiquidityReturnType {
  const { sender, amount, minSharesToMint } = parameters;

  const { addresses } = await getAppConfig(client);

  const msg = {
    vault: {
      addLiquidity: {
        amount,
        ...(minSharesToMint ? { minSharesToMint } : {}),
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
