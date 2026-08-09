import { getAppConfig } from "#actions/app/queries/getAppConfig.js";
import { execute } from "#actions/app/mutations/execute.js";

import type { Address, Client, Signer } from "@left-curve/types";
import type { SignAndBroadcastTxReturnType } from "#actions/app/mutations/signAndBroadcastTx.js";

export type VaultRemoveLiquidityParameters = {
  sender: Address;
  sharesToBurn: string;
};

export type VaultRemoveLiquidityReturnType = SignAndBroadcastTxReturnType;

export async function vaultRemoveLiquidity(
  client: Client<Signer>,
  parameters: VaultRemoveLiquidityParameters,
): VaultRemoveLiquidityReturnType {
  const { sender, sharesToBurn } = parameters;

  const { addresses } = await getAppConfig(client);

  const msg = {
    vault: {
      removeLiquidity: {
        sharesToBurn,
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
