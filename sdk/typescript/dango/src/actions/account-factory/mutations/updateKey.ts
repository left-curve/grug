import { getAppConfig } from "#actions/app/queries/getAppConfig.js";
import type { Address, Hex } from "@left-curve/types";
import { type ExecuteReturnType, execute } from "#actions/app/mutations/execute.js";

import type { Client, Key, KeyHash, Signer } from "@left-curve/types";

export type UpdateKeyParameters = {
  sender: Address;
  action: "delete" | { insert: Key };
  keyHash: KeyHash;
};

export type UpdateKeyReturnType = ExecuteReturnType;

export type MsgUpdateKey = {
  updateKey: {
    KeyHash: Hex;
    key: "delete" | { insert: Key };
  };
};

export async function updateKey(
  client: Client<Signer>,
  parameters: UpdateKeyParameters,
): UpdateKeyReturnType {
  const { keyHash, action, sender } = parameters;

  const { addresses } = await getAppConfig(client);

  const UpdateKeyMsg = {
    updateKey: {
      keyHash,
      key: action,
    },
  };

  return await execute(client, {
    execute: {
      contract: addresses.accountFactory,
      msg: UpdateKeyMsg,
    },
    sender,
  });
}
