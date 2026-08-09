import type { Address, Client, GetTxMessage, Signer } from "@left-curve/types";
import { type SignAndBroadcastTxReturnType, signAndBroadcastTx } from "./signAndBroadcastTx.js";

type Message = GetTxMessage<"upgrade">;

export type UpgradeParameters = {
  sender: Address;
} & Message["upgrade"];

export type UpgradeReturnType = Promise<SignAndBroadcastTxReturnType>;

export async function upgrade(
  client: Client<Signer>,
  parameters: UpgradeParameters,
): UpgradeReturnType {
  const { sender, height, cargoVersion, gitTag, url } = parameters;

  const message: Message = {
    upgrade: {
      height,
      cargoVersion,
      gitTag,
      url,
    },
  };

  return await signAndBroadcastTx(client, { sender, messages: [message] });
}
