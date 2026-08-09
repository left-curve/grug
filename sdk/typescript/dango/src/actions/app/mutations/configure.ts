import type { Address, Client, GetTxMessage, Signer } from "@left-curve/types";
import { type SignAndBroadcastTxReturnType, signAndBroadcastTx } from "./signAndBroadcastTx.js";

type Message = GetTxMessage<"configure">;

export type ConfigureParameters = {
  sender: Address;
} & Message["configure"];

export type ConfigureReturnType = Promise<SignAndBroadcastTxReturnType>;

export async function configure(
  client: Client<Signer>,
  parameters: ConfigureParameters,
): ConfigureReturnType {
  const { sender, newAppCfg, newCfg } = parameters;

  const message: Message = {
    configure: {
      newAppCfg,
      newCfg,
    },
  };

  return await signAndBroadcastTx(client, { sender, messages: [message] });
}
