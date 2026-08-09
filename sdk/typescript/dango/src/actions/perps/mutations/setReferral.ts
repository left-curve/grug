import { getAppConfig } from "#actions/app/queries/getAppConfig.js";
import { execute } from "#actions/app/mutations/execute.js";

import type { Address, Client, Signer } from "@left-curve/types";
import type { SignAndBroadcastTxReturnType } from "#actions/app/mutations/signAndBroadcastTx.js";

export type SetReferralParameters = {
  sender: Address;
  referrer: number;
  referee: number;
};

export type SetReferralReturnType = SignAndBroadcastTxReturnType;

export async function setReferral(
  client: Client<Signer>,
  parameters: SetReferralParameters,
): SetReferralReturnType {
  const { sender, referrer, referee } = parameters;

  const { addresses } = await getAppConfig(client);

  const msg = {
    referral: {
      setReferral: {
        referrer,
        referee,
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
